# Ongoing Tasks

## Completed: User Guide Update + Multi-Stage Example

### Summary

Updated `docs/USER_GUIDE.md` build section and created `examples/multi-stage/` to document
all new build features (ARG, ADD, multi-stage, .remignore).

### Changes Made

1. **`docs/USER_GUIDE.md`**: Expanded instruction table (FROM with AS, COPY with --from,
   ADD, ARG, ENTRYPOINT, LABEL, USER); added subsections for ARG, ADD vs COPY, multi-stage
   builds, and .remignore; added --build-arg and --no-cache flags; added multi-stage build
   example section
2. **`examples/multi-stage/Remfile`**: Two-stage build (builder + final) with ARG PROFILE
3. **`examples/multi-stage/src/main.rs`**: Tiny HTTP server (~35 lines, std-only)
4. **`examples/multi-stage/Cargo.toml`**: Minimal manifest
5. **`examples/multi-stage/.remignore`**: Excludes target/, .git/, *.md
6. **`examples/multi-stage/README.md`**: Usage and explanation

## Next Task

(No next task planned — awaiting user direction.)
