# Bundled tools

`src/tools.rs` embeds the scripts here and installs them under
`~/.clud/tools/`. Invoke one with `clud tool run <relative-path> [args]`.

The runner returns the tool's own exit code when the tool finishes. Its
watchdog returns `124` when a time limit fires. A resumable tool gets a JSON
line on stderr with `status: in-progress`; run it again with the same args.
A killable tool gets `status: aborted`. Exit `0` means the tool itself
finished successfully. The watchdog passes its command cap to children as
`CLUD_TOOL_COMMAND_TIMEOUT_SECS`; `github/pr_merge_watch.py` uses that value
to finish its own timeout and cleanup before the wrapper's cap.
