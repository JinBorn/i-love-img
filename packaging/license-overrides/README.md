# Upstream license supplements

Some published crates omit license files from their source archives. These copies fill those gaps and preserve native MozJPEG license material excluded from its crate archive. `packages.json` maps exact crate versions to directories; `sources.json` records the original URL and SHA-256 of every downloaded file.

GitHub links use the commit recorded by each crate's `.cargo_vcs_info.json`. MozJPEG uses the `vendor` git submodule commit in mozjpeg-sys 2.2.3. hexf-parse declares CC0-1.0 without shipping the full text; its pinned README is accompanied by the complete CC0 text from Creative Commons. dispatch 0.2.0 declares MIT without a separate upstream notice file; its original Cargo.toml and the standard SPDX MIT reference text are included with an explicit explanatory notice, without fabricating a copyright holder or year.

The collector copies these supplements under each dependency's `upstream/` directory and records their provenance in that dependency's `package.json`. It does not access the network to fetch notices during packaging. Review and update these mappings when upgrading dependencies. epaint's font notices are copied directly from the crate's `fonts/` directory.
