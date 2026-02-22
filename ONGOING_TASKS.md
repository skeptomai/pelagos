# Ongoing Tasks

## Current: Image Build Enhancements (ARG, .remignore, ADD, Multi-stage)

### Context

The build engine (`src/build.rs`) supports FROM, RUN, COPY, CMD, ENTRYPOINT, ENV, WORKDIR, EXPOSE, LABEL, USER with a working build cache. Four features remain: ARG, ADD, multi-stage builds, and `.remignore`.

### Commit 1: ARG instruction + variable substitution

- Add `Arg { name, default }` variant to `Instruction`
- Parser: `ARG NAME=default` or bare `NAME`; allowed before FROM
- `substitute_vars(text, vars) -> String`: replace `$VAR` / `${VAR}`, `$$` → literal `$`
- `substitute_instruction(instr, vars) -> Instruction`: clone with substitution
- Build state: `args_map: HashMap<String, String>` seeded from CLI `--build-arg`
- `execute_build()` signature: add `build_args: &HashMap<String, String>`
- CLI: `--build-arg KEY=VALUE` flag
- Tests: unit + integration

### Commit 2: .remignore support

- Add `ignore = "0.4"` dependency
- `load_remignore(context_dir) -> Option<Gitignore>`
- `copy_dir_filtered(src, dst, ignore, src_root)` — skip matching entries
- Wire into `execute_copy()` with optional ignore param
- Tests: unit + integration

### Commit 3: ADD instruction

- Add `ureq`, `bzip2`, `xz2` dependencies
- `Add { src, dest }` variant
- URL download, archive extraction (.tar, .tar.gz, .tar.bz2, .tar.xz)
- `execute_add()` dispatch: URL → download, archive → extract, else → copy
- Tests: unit + integration

### Commit 4: Multi-stage builds

- `From { image, alias }` and `Copy { src, dest, from_stage }` variants
- FROM: parse `AS alias`; COPY: parse `--from=name`
- `split_into_stages()`, `execute_stage()`
- COPY --from: walk stage layers to find source file
- Update all pattern matches (~15+ locations)
- Tests: unit + integration

### Verification

After each commit: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --lib`
After all 4: user runs `sudo -E cargo test --test integration_tests`
