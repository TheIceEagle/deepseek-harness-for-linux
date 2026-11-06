# Native client third-party software

This native client is an independent implementation. It does not bundle the
upstream Harness JavaScript runtime, Node.js, or a WebView. DeepSeek's name
identifies a supported model provider; this project is not affiliated with or
endorsed by DeepSeek.

The application is MIT licensed. Rust dependencies retain their own licenses;
Cargo.lock identifies the reviewed versions. Debian packaging copies the
available LICENSE, COPYING and NOTICE files from the production dependency graph
into `third-party-licenses/` beside this notice and includes an index with each
package's declared license expression. This also includes notices accompanying
bundled third-party source within those crates.

GTK4 and its related native libraries are dynamically linked system packages.
Their licenses and notices are supplied by the operating system's packages.
The included Uppercase demonstration extension is part of this MIT-licensed
project and is installed only when the user chooses it.
