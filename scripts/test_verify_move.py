#!/usr/bin/env python3
"""Unit tests for scripts/verify-move.py (#479): the scanner cases where a naive splitter is wrong, and the
verdicts (lost / added / changed / identical).

Run:  python -m unittest scripts/test_verify_move.py   (or: python scripts/test_verify_move.py)
"""
import importlib.util
import os
import unittest

_here = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("verify_move", os.path.join(_here, "verify-move.py"))
vm = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vm)


def keys(src):
    return sorted(vm.label(k) for k in vm.collect(src))


class Splitting(unittest.TestCase):
    def test_functions_structs_and_impls_are_separate_items(self):
        src = "fn a() {}\nstruct S { x: u8 }\nimpl S { fn m(&self) {} }\nstruct U;\n"
        self.assertEqual(keys(src), ["fn a", "impl S", "struct S", "struct U"])

    def test_comments_and_attributes_stay_with_their_item(self):
        src = "/// doc\n#[derive(Debug)]\npub struct S;\n\n// plain\nfn f() {}\n"
        chunks = vm.split_items(src)
        self.assertEqual(len(chunks), 2)
        self.assertIn("/// doc", chunks[0])
        self.assertIn("// plain", chunks[1])

    def test_a_semicolon_inside_an_array_type_does_not_end_the_item(self):
        self.assertEqual(keys("fn f(a: [u8; 4]) -> [u8; 2] { [0; 2] }\nconst K: [u8; 2] = [1; 2];\n"),
                         ["const K", "fn f"])

    def test_braces_and_semicolons_in_strings_and_comments_are_ignored(self):
        src = 'fn f() { let s = "}; fn g() {"; /* } */ let c = \'}\'; }\nfn h() {}\n'
        self.assertEqual(keys(src), ["fn f", "fn h"])

    def test_raw_strings_and_lifetimes(self):
        src = 'fn f<\'a>(x: &\'a str) -> &\'a str { let _r = r#"} fn z() {"#; x }\nfn g() {}\n'
        self.assertEqual(keys(src), ["fn f", "fn g"])

    def test_use_with_braces_ends_at_its_semicolon(self):
        self.assertEqual(len(vm.split_items("use a::{b, c};\nfn f() {}\n")), 2)

    def test_const_with_a_struct_literal_ends_at_its_semicolon(self):
        self.assertEqual(keys("const C: P = P { x: 1 };\nfn f() {}\n"), ["const C", "fn f"])

    def test_inline_mod_bodies_are_recursed_with_a_prefix(self):
        src = "#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn t() {}\n}\n"
        self.assertEqual(keys(src), ["fn tests::t", "use tests::super::*"])

    def test_cfg_variants_of_one_function_have_distinct_keys(self):
        src = '#[cfg(unix)]\nfn f() {}\n#[cfg(windows)]\nfn f() {}\n'
        self.assertEqual(len(vm.collect(src)), 2)

    def test_macro_rules_is_an_item(self):
        self.assertEqual(keys("macro_rules! m { () => {}; }\nfn f() {}\n"), ["fn f", "macro_rules! m"])


class Verdicts(unittest.TestCase):
    OLD = "pub fn a() -> u8 {\n    1\n}\n\nfn b() {}\n"

    def test_identical_move_passes(self):
        lost, added, changed, _ = vm.compare(self.OLD, "fn b() {}\npub fn a() -> u8 {\n    1\n}\n")
        self.assertEqual((lost, added, changed), ([], [], []))

    def test_visibility_prefixes_are_normalised(self):
        new = "pub(crate) fn a() -> u8 {\n    1\n}\npub(super) fn b() {}\n"
        self.assertEqual(vm.compare(self.OLD, new)[:3], ([], [], []))

    def test_moving_into_a_mod_only_changes_indentation(self):
        new = "mod inner {\n    pub fn a() -> u8 {\n        1\n    }\n\n    fn b() {}\n}\n"
        lost, added, changed, _ = vm.compare(self.OLD, new)
        # keys differ by the `inner::` prefix: the move into a mod is reported, not silently accepted.
        self.assertEqual(len(lost), 2)
        self.assertEqual(len(added), 2)
        self.assertEqual(changed, [])

    def test_lost_item_is_reported(self):
        lost, added, changed, _ = vm.compare(self.OLD, "fn b() {}\n")
        self.assertEqual([vm.label(k) for k in lost], ["fn a"])
        self.assertEqual((added, changed), ([], []))

    def test_added_item_is_reported(self):
        lost, added, _, _ = vm.compare(self.OLD, self.OLD + "\nfn extra() {}\n")
        self.assertEqual([vm.label(k) for k in added], ["fn extra"])
        self.assertEqual(lost, [])

    def test_a_changed_body_is_reported_even_for_one_character(self):
        new = "pub fn a() -> u8 {\n    2\n}\n\nfn b() {}\n"
        _, _, changed, _ = vm.compare(self.OLD, new)
        self.assertEqual([vm.label(k) for k, _, _ in changed], ["fn a"])

    def test_use_items_are_not_compared(self):
        old = "use a::b;\nfn f() {}\n"
        new = "use c::d;\nuse e::f;\nfn f() {}\n"
        self.assertEqual(vm.compare(old, new)[:3], ([], [], []))

    def test_rename_substitution_applies_to_the_old_text(self):
        old = "fn f() { crate::old::g() }\n"
        new = "fn f() { crate::new::g() }\n"
        self.assertEqual(vm.compare(old, new)[2] != [], True)
        self.assertEqual(vm.compare(old, new, renames=[("old::", "new::")])[:3], ([], [], []))

    def test_allow_ignores_matching_items(self):
        lost, added, _, _ = vm.compare("fn a() {}\n", "fn z() {}\n", allow=["fn a", "fn z"])
        self.assertEqual((lost, added), ([], []))

    def test_repeated_impl_blocks_match_as_a_multiset(self):
        old = "impl S { fn a(&self) {} }\nimpl S { fn b(&self) {} }\n"
        new = "impl S { fn b(&self) {} }\nimpl S { fn a(&self) {} }\n"
        self.assertEqual(vm.compare(old, new)[:3], ([], [], []))
        _, _, changed, _ = vm.compare(old, "impl S { fn a(&self) {} }\nimpl S { fn c(&self) {} }\n")
        self.assertEqual(len(changed), 1)


if __name__ == "__main__":
    unittest.main()
