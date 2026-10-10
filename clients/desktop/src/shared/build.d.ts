// The commit this bundle was built from, replaced by esbuild at build time.
//
// Declared rather than imported because it is not a module: `build.mjs`
// substitutes the literal, so nothing is read at runtime and nothing has to
// exist for a test to run.
declare const OVS_BUILD_COMMIT: string;
