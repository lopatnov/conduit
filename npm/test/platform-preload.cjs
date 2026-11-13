// Preloaded into download.js by the per-platform test (node -r): makes process.platform and process.arch read as the
// platform named in CONDUIT_TEST_PLATFORM ("linux-arm64", "linux-x64-musl"), and on Linux makes the runtime report
// say which libc it is, so every row of the platform table is exercised on any host.
// A third part "v6" or "v7" is the ARM level Node reports in `process.config.variables.arm_version` ("linux-arm-v7").
const [platform, arch, libc] = (process.env.CONDUIT_TEST_PLATFORM || "").split("-");
if (platform && arch) {
  Object.defineProperty(process, "platform", { value: platform });
  Object.defineProperty(process, "arch", { value: arch });
  if (arch === "arm") {
    // Every synthetic ARM case sets the ARM level itself: the one in the suffix, or none at all, never the host's
    // (an ARMv7 host would otherwise make "linux-arm" read as ARMv7).
    const variables = { ...process.config.variables };
    if (/^v\d$/.test(libc || "")) variables.arm_version = libc.slice(1);
    else delete variables.arm_version;
    Object.defineProperty(process, "config", { value: { ...process.config, variables } });
  }
  if (platform === "linux") {
    const header = libc === "musl" ? {} : { glibcVersionRuntime: "2.39" };
    Object.defineProperty(process.report, "getReport", { value: () => ({ header }) });
  }
}
