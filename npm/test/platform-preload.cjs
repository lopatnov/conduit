// Preloaded into download.js by the per-platform test (node -r): makes process.platform and process.arch read as the
// platform named in CONDUIT_TEST_PLATFORM ("linux-arm64"), so every row of the platform table is exercised on any host.
const [platform, arch] = (process.env.CONDUIT_TEST_PLATFORM || "").split("-");
if (platform && arch) {
  Object.defineProperty(process, "platform", { value: platform });
  Object.defineProperty(process, "arch", { value: arch });
}
