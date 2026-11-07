//! Which host functions each phase provides, and what `conduit validate` can say about a plugin file before it
//! serves a request (issue #565).
//!
//! The request phase links a module against 17 host functions and the response phase against 7 (20 distinct
//! names). A module that has both `on_request` and `on_response` is linked once per phase, and a module that
//! imports a function its phase does not define does not instantiate at all. Until #565 that made a plugin
//! that read a header in `on_request` and the upstream status in `on_response` fail open on every request and
//! every response. Now an import that belongs to the *other* phase is defined as a function that traps if it
//! is called, so the module links in both phases and only a call made in the wrong phase fails.

use std::collections::HashSet;

use wasmtime::{ExternType, Linker, Module, ValType};

use crate::wasm::{
    engine, read_module_file, register_host_functions, register_response_host_functions,
    WasmResponseState, WasmState, MEMORY_LIMIT_BYTES,
};

/// The size of a WebAssembly memory page.
const WASM_PAGE_BYTES: u64 = 65_536;

/// Host functions the request phase defines and the response phase does not.
pub(crate) const REQUEST_ONLY: &[&str] = &[
    "conduit_abort_with_redirect",
    "conduit_get_client_ip",
    "conduit_get_header",
    "conduit_get_header_count",
    "conduit_get_header_names",
    "conduit_get_method",
    "conduit_get_path",
    "conduit_get_query",
    "conduit_get_request_id",
    "conduit_get_uri",
    "conduit_remove_request_header",
    "conduit_set_request_header",
    "conduit_set_response_status",
];

/// Host functions the response phase defines and the request phase does not.
pub(crate) const RESPONSE_ONLY: &[&str] = &[
    "conduit_get_response_header",
    "conduit_get_response_status",
    "conduit_remove_response_header",
];

/// Host functions that take no pointer, so a module may call them without exporting its memory.
const WITHOUT_MEMORY: &[&str] = &[
    "conduit_get_header_count",
    "conduit_get_response_status",
    "conduit_set_response_status",
];

/// Define, for every import of `module` that belongs to the other phase (`unavailable`), a function of the
/// type the module declares that traps with a clear message when it is called.
pub(crate) fn define_unavailable<T: 'static>(
    linker: &mut Linker<T>,
    module: &Module,
    unavailable: &[&str],
) -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    for import in module.imports() {
        if import.module() != "conduit" || !unavailable.contains(&import.name()) {
            continue;
        }
        let ExternType::Func(ty) = import.ty() else {
            continue;
        };
        let name = import.name().to_owned();
        if !seen.insert(name.clone()) {
            continue;
        }
        let message = format!("{name} is not available in this phase");
        linker.func_new("conduit", &name, ty, move |_, _, _| {
            Err(wasmtime::Error::msg(message.clone()))
        })?;
    }
    Ok(())
}

/// What is wrong with a plugin file, as the text a person should read: the kind of I/O error (not the operating
/// system's own wording, which differs between platforms), or the engine's message.
#[derive(Debug, PartialEq, Eq)]
pub enum ModuleProblem {
    /// The plugin cannot run at all: at run time it is skipped and the request passes through.
    Unusable(String),
    /// The plugin runs, but a part of what it asks of the host does not work, so it does not behave as written.
    Degraded(String),
}

impl ModuleProblem {
    /// The text of the problem.
    pub fn message(&self) -> &str {
        match self {
            Self::Unusable(why) | Self::Degraded(why) => why,
        }
    }
}

impl std::fmt::Display for ModuleProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// What can be said about the plugin file at `path` before it serves a request: it reads, compiles, exports
/// `on_request` as `() -> i32` and links in the phases it uses ([`ModuleProblem::Unusable`] when not), and it
/// exports its memory when a function it imports needs it ([`ModuleProblem::Degraded`] when not: such a plugin
/// runs, but its calls that read or write memory see nothing). Nothing in the module runs.
///
/// What this cannot tell: whether the plugin does the right thing, or traps at run time.
pub fn check_module(path: &str) -> Result<(), ModuleProblem> {
    let module = load_for_check(path).map_err(ModuleProblem::Unusable)?;
    // An `on_response` of another type is not called (the host asks for `(i32) -> i32` and skips it silently), but
    // `on_request` and the rest of the plugin work.
    match module.get_export("on_response") {
        Some(ExternType::Func(f)) if has_response_type(&f) => {}
        Some(_) => {
            return Err(ModuleProblem::Degraded(
                "exports on_response with a type other than (i32) -> i32, so on_response is never called".to_owned(),
            ));
        }
        None => {}
    }
    let needs_memory = module
        .imports()
        .any(|i| i.module() == "conduit" && !WITHOUT_MEMORY.contains(&i.name()));
    if needs_memory && !matches!(module.get_export("memory"), Some(ExternType::Memory(_))) {
        return Err(ModuleProblem::Degraded(
            "imports host functions that read or write its memory but does not export a memory named \"memory\", \
             so those calls read nothing and write nothing"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Read, compile and link the module for [`check_module`]; the reason when it cannot run.
fn load_for_check(path: &str) -> Result<Module, String> {
    let bytes = read_module_file(path).map_err(|e| e.describe())?;
    let engine = engine().map_err(|e| e.to_string())?;
    let module = Module::new(engine, &bytes)
        .map_err(|e| format!("is not a valid WebAssembly module: {e:#}"))?;
    // A memory the host cannot give the plugin is a failure to instantiate on every call (fail-open). Only an
    // exported memory can be looked at from outside, and a plugin that uses the host needs to export it.
    if let Some(ExternType::Memory(memory)) = module.get_export("memory") {
        if memory.is_shared() {
            return Err("declares a shared memory, which this host does not enable, so it cannot be instantiated".to_owned());
        }
        let initial = memory.minimum().saturating_mul(WASM_PAGE_BYTES);
        if initial > MEMORY_LIMIT_BYTES as u64 {
            return Err(format!(
                "declares an initial memory of {} MiB, over the {} MiB limit, so it cannot be instantiated",
                initial / (1024 * 1024),
                MEMORY_LIMIT_BYTES / (1024 * 1024)
            ));
        }
    }

    match module.get_export("on_request") {
        Some(ExternType::Func(f)) if f.params().len() == 0 && is_one(f.results(), ValType::I32) => {}
        Some(_) => return Err("exports on_request with a type other than () -> i32".to_owned()),
        None => return Err("does not export on_request (it is required even in a plugin that only uses on_response)".to_owned()),
    }
    // Only an `on_response` of the right type is ever called, so only that one needs the response phase to link.
    let has_on_response = matches!(
        module.get_export("on_response"),
        Some(ExternType::Func(f)) if has_response_type(&f)
    );

    let mut request = Linker::<WasmState>::new(engine);
    register_host_functions(&mut request).map_err(|e| e.to_string())?;
    define_unavailable(&mut request, &module, RESPONSE_ONLY).map_err(|e| e.to_string())?;
    request
        .instantiate_pre(&module)
        .map_err(|e| format!("cannot be linked: {e:#}"))?;
    if has_on_response {
        let mut response = Linker::<WasmResponseState>::new(engine);
        register_response_host_functions(&mut response).map_err(|e| e.to_string())?;
        define_unavailable(&mut response, &module, REQUEST_ONLY).map_err(|e| e.to_string())?;
        response
            .instantiate_pre(&module)
            .map_err(|e| format!("cannot be linked: {e:#}"))?;
    }

    Ok(module)
}

/// True when `f` is `(i32) -> i32`, the type the host calls `on_response` with.
fn has_response_type(f: &wasmtime::FuncType) -> bool {
    is_one(f.params(), ValType::I32) && is_one(f.results(), ValType::I32)
}

/// True when `types` is exactly one value of type `want`.
fn is_one(mut types: impl ExactSizeIterator<Item = ValType>, want: ValType) -> bool {
    types.len() == 1 && types.next().is_some_and(|t| ValType::eq(&t, &want))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wasm::{WasmRequest, WasmResponseContext};
    use std::collections::HashMap;
    use std::io::Write as _;

    fn wasm_file(wat_src: &str) -> (tempfile::NamedTempFile, String) {
        let bytes = wat::parse_str(wat_src).expect("WAT parse");
        let mut f = tempfile::Builder::new().suffix(".wasm").tempfile().unwrap();
        f.write_all(&bytes).unwrap();
        f.flush().unwrap();
        let path = f.path().to_string_lossy().into_owned();
        (f, path)
    }

    fn names<T: 'static>(linker: &Linker<T>, store: &mut wasmtime::Store<T>) -> HashSet<String> {
        linker
            .iter(&mut *store)
            .filter(|(module, _, _)| *module == "conduit")
            .map(|(_, name, _)| name.to_owned())
            .collect()
    }

    /// `REQUEST_ONLY` and `RESPONSE_ONLY` must be exactly what one phase defines and the other does not;
    /// a host function added to one phase without updating them would make a module that imports it fail
    /// to link in the other phase.
    #[test]
    fn the_phase_only_lists_match_the_linkers() {
        let engine = engine().unwrap();
        let request = WasmRequest {
            method: String::new(),
            path: String::new(),
            query: String::new(),
            client_ip: String::new(),
            headers: HashMap::new(),
            header_names: Vec::new(),
            request_id: String::new(),
            plugin_config: Vec::new(),
        };
        let mut request_linker = Linker::<WasmState>::new(engine);
        register_host_functions(&mut request_linker).unwrap();
        let mut request_store = wasmtime::Store::new(engine, WasmState::new(request));
        let request_names = names(&request_linker, &mut request_store);

        let mut response_linker = Linker::<WasmResponseState>::new(engine);
        register_response_host_functions(&mut response_linker).unwrap();
        let ctx = WasmResponseContext {
            status: 200,
            headers: HashMap::new(),
            plugin_config: Vec::new(),
        };
        let mut response_store = wasmtime::Store::new(engine, WasmResponseState::new(ctx));
        let response_names = names(&response_linker, &mut response_store);

        let sorted = |set: HashSet<String>| {
            let mut v: Vec<String> = set.into_iter().collect();
            v.sort();
            v
        };
        let strings = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<HashSet<_>>();
        assert_eq!(
            sorted(request_names.difference(&response_names).cloned().collect()),
            sorted(strings(REQUEST_ONLY))
        );
        assert_eq!(
            sorted(response_names.difference(&request_names).cloned().collect()),
            sorted(strings(RESPONSE_ONLY))
        );
        assert_eq!((request_names.len(), response_names.len()), (17, 7));
        for name in WITHOUT_MEMORY {
            assert!(
                request_names.contains(*name) || response_names.contains(*name),
                "{name} is not a host function"
            );
        }
    }

    const GOOD: &str = r#"(module
        (import "conduit" "conduit_get_header" (func (param i32 i32 i32 i32) (result i32)))
        (import "conduit" "conduit_get_response_status" (func (result i32)))
        (memory (export "memory") 1)
        (func (export "on_request") (result i32) i32.const 0)
        (func (export "on_response") (param i32) (result i32) i32.const 0))"#;

    fn problem(wat_src: &str) -> String {
        let (_f, path) = wasm_file(wat_src);
        check_module(&path)
            .expect_err("the module should be rejected")
            .to_string()
    }

    #[test]
    fn a_module_that_uses_both_phases_is_accepted() {
        let (_f, path) = wasm_file(GOOD);
        assert_eq!(check_module(&path), Ok(()));
    }

    #[test]
    fn the_smallest_plugin_is_accepted() {
        let (_f, path) =
            wasm_file(r#"(module (func (export "on_request") (result i32) i32.const 0))"#);
        assert_eq!(
            check_module(&path),
            Ok(()),
            "no import needs a memory, so none is required"
        );
    }

    #[test]
    fn a_missing_file_is_named_by_kind() {
        assert_eq!(
            check_module("/no/such/plugin.wasm"),
            Err(ModuleProblem::Unusable(
                "cannot be read (entity not found)".to_owned()
            ))
        );
    }

    #[test]
    fn a_file_that_is_not_webassembly_is_rejected() {
        let mut f = tempfile::Builder::new().suffix(".wasm").tempfile().unwrap();
        f.write_all(b"(module)").unwrap();
        let path = f.path().to_string_lossy().into_owned();
        // WAT text is not accepted: the host reads binary modules only.
        let why = check_module(&path).unwrap_err().to_string();
        assert!(
            why.starts_with("is not a valid WebAssembly module"),
            "{why}"
        );
    }

    #[test]
    fn on_request_is_required() {
        let why = problem(
            r#"(module (func (export "on_response") (param i32) (result i32) i32.const 0))"#,
        );
        assert!(why.starts_with("does not export on_request"), "{why}");
    }

    #[test]
    fn on_request_must_have_the_right_type() {
        let why = problem(
            r#"(module (func (export "on_request") (param i32) (result i32) i32.const 0))"#,
        );
        assert_eq!(why, "exports on_request with a type other than () -> i32");
    }

    #[test]
    fn on_response_with_the_wrong_type_is_reported_because_it_is_never_called() {
        let why = problem(
            r#"(module
                (func (export "on_request") (result i32) i32.const 0)
                (func (export "on_response") (result i32) i32.const 0))"#,
        );
        assert!(
            why.contains("on_response") && why.contains("never called"),
            "{why}"
        );
    }

    #[test]
    fn a_wasi_import_does_not_link() {
        let why = problem(
            r#"(module
                (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
                (func (export "on_request") (result i32) i32.const 0))"#,
        );
        assert!(
            why.starts_with("cannot be linked") && why.contains("fd_write"),
            "{why}"
        );
    }

    #[test]
    fn a_misspelled_conduit_function_does_not_link() {
        let why = problem(
            r#"(module
                (import "conduit" "conduit_get_hedaer" (func (param i32 i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (func (export "on_request") (result i32) i32.const 0))"#,
        );
        assert!(
            why.starts_with("cannot be linked") && why.contains("conduit_get_hedaer"),
            "{why}"
        );
    }

    #[test]
    fn a_host_function_declared_with_the_wrong_type_does_not_link() {
        let why = problem(
            r#"(module
                (import "conduit" "conduit_get_header" (func (param i32) (result i32)))
                (memory (export "memory") 1)
                (func (export "on_request") (result i32) i32.const 0))"#,
        );
        assert!(why.starts_with("cannot be linked"), "{why}");
    }

    #[test]
    fn a_response_function_declared_with_the_wrong_type_is_caught_through_on_response() {
        let why = problem(
            r#"(module
                (import "conduit" "conduit_get_response_status" (func (param i32) (result i32)))
                (func (export "on_request") (result i32) i32.const 0)
                (func (export "on_response") (param i32) (result i32) i32.const 0))"#,
        );
        assert!(why.starts_with("cannot be linked"), "{why}");
    }

    #[test]
    fn a_function_that_needs_memory_without_an_exported_memory_is_reported() {
        let why = problem(
            r#"(module
                (import "conduit" "conduit_get_path" (func (param i32 i32) (result i32)))
                (func (export "on_request") (result i32) i32.const 0))"#,
        );
        assert!(why.contains("does not export a memory"), "{why}");
        let (_f, path) = wasm_file(
            r#"(module
                (import "conduit" "conduit_get_path" (func (param i32 i32) (result i32)))
                (func (export "on_request") (result i32) i32.const 0))"#,
        );
        assert!(
            matches!(check_module(&path), Err(ModuleProblem::Degraded(_))),
            "such a plugin runs, so it is not reported as unusable"
        );
    }

    #[test]
    fn functions_without_a_pointer_need_no_memory() {
        let (_f, path) = wasm_file(
            r#"(module
                (import "conduit" "conduit_set_response_status" (func (param i32)))
                (func (export "on_request") (result i32) i32.const 403 call 0 i32.const 1))"#,
        );
        assert_eq!(check_module(&path), Ok(()));
    }

    #[test]
    fn an_initial_memory_over_the_limit_is_reported() {
        let why = problem(
            r#"(module (memory (export "memory") 257) (func (export "on_request") (result i32) i32.const 0))"#,
        );
        assert!(why.contains("over the 16 MiB limit"), "{why}");
        let (_f, path) = wasm_file(
            r#"(module (memory (export "memory") 256) (func (export "on_request") (result i32) i32.const 0))"#,
        );
        assert_eq!(check_module(&path), Ok(()), "exactly the limit is allowed");
    }

    #[test]
    fn a_shared_memory_is_reported() {
        let why = problem(
            r#"(module (memory (export "memory") 1 1 shared) (func (export "on_request") (result i32) i32.const 0))"#,
        );
        assert!(why.contains("shared"), "{why}");
    }

    #[test]
    fn an_on_response_of_the_wrong_type_is_degraded_not_unusable() {
        let (_f, path) = wasm_file(
            r#"(module
                (func (export "on_request") (result i32) i32.const 0)
                (func (export "on_response") (result i32) i32.const 0))"#,
        );
        assert!(
            matches!(check_module(&path), Err(ModuleProblem::Degraded(_))),
            "on_request still runs"
        );
    }

    #[test]
    fn a_directory_and_an_oversized_file_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let why = check_module(dir.path().to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert_eq!(why, "is not a regular file");
        let big = dir.path().join("big.wasm");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(64 * 1024 * 1024 + 1)
            .unwrap();
        let why = check_module(big.to_str().unwrap()).unwrap_err().to_string();
        assert_eq!(why, "is larger than 64 MiB");
    }
}
