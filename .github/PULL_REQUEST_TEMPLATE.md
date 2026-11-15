<!-- Thank you! Please read CONTRIBUTING.md first. -->

**What and why**


**How tested**
<!-- Commands, cameras/files, screenshots for interface changes. Tested with an empty data folder (GRAINEO_DATA_DIR)? -->


**Checklist**
- [ ] Every commit is signed off (`git commit -s`, Developer Certificate of Origin)
- [ ] `cargo build --release -p graineo` and the tests of the touched crates pass
- [ ] New user-visible texts use `crate::tr` and have an English entry in `crates/app/locales/en-ui.tsv`
- [ ] Touches `crates/x3f`, `crates/develop` or `third_party/x3fuse-core`: noted here, so the Sigma checksum check can be run
- [ ] Third-party code or data: source and licence noted in the code and in `THIRD_PARTY.md`
