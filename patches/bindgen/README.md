# Parser build dependency patch

`postgres-parser` 0.2.3 requires bindgen 0.57 with its default features. That
pulls in vulnerable `atty` through the unused command-line and logging features,
and an unpatched shlex 0.1 dependency. There is no newer published parser release.

This local compatibility crate re-exports bindgen 0.59.2, which retains the APIs
used by the parser's build script and supports patched shlex 1.3. Only runtime
libclang loading is enabled; the CLI and logger dependencies are unnecessary
for generating parser bindings.

Remove this patch when the parser publishes an updated build dependency.
