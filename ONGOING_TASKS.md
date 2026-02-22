# Ongoing Tasks

## Completed: Image Build Enhancements (ARG, .remignore, ADD, Multi-stage)

### Summary

Added four build engine features across 4 commits:

1. **ARG instruction + variable substitution**: `ARG NAME=default` with `$VAR`/`${VAR}` substitution, `--build-arg` CLI flag, ARG before FROM (Docker compat)
2. **`.remignore` support**: gitignore-style context filtering via `ignore` crate, `copy_dir_filtered()`, wired into COPY and ADD
3. **ADD instruction**: URL download (ureq), archive auto-extraction (.tar/.tar.gz/.tar.bz2/.tar.xz), plain copy fallback
4. **Multi-stage builds**: `FROM ... AS alias`, `COPY --from=stage`, `split_into_stages()`, `execute_stage()`, layer-walk for cross-stage copies

### Changes Made

1. **`Cargo.toml`**: Added `ignore`, `ureq`, `bzip2`, `xz2` dependencies
2. **`src/build.rs`**: ARG variant, ADD variant, From/Copy variant restructuring, `substitute_vars()`, `substitute_instruction()`, `load_remignore()`, `copy_dir_filtered()`, `execute_add()`, `execute_add_url()`, `execute_add_archive()`, `BuildStage`, `split_into_stages()`, `execute_stage()`, `execute_copy_from_stage()`, 13 new unit tests
3. **`src/cli/build.rs`**: `--build-arg` flag, HashMap parsing
4. **`src/image.rs`**: Added `Default` derive to `ImageConfig`
5. **`tests/integration_tests.rs`**: 5 new integration tests (parser + filtering)
6. **`docs/INTEGRATION_TESTS.md`**: Documented all 5 new tests
7. **`CLAUDE.md`**: Updated build features section
8. **`docs/ROADMAP.md`**: Removed completed "Image Build Enhancements" from planned section

## Next Task

(No next task planned — awaiting user direction.)
