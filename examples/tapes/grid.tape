# The result grid: 50,000 rows streamed in, a cell cursor, selecting a row,
# resizing a column and copying as JSON.
Output examples/demo/grid.gif
Source examples/tapes/common.tape

# Each tape starts with an empty history.
Hide
Type "rm -f target/demo/state/history.jsonl && clear" Enter
Show

Type "./target/release/dfox-tui" Enter
Sleep 2s
Enter
Sleep 1s

Type "select * from orders"
Ctrl+E
Sleep 1.5s
Tab
PageDown@300ms 3
Right@150ms 3
Sleep 400ms
Type "V"
Sleep 800ms
Type ">>"
Sleep 600ms
Type "y"
Sleep 800ms
Type "j"
Sleep 1.5s
# Quit off camera, so the GIF ends on dfox, not the shell.
Hide
Ctrl+Q
Type "y"
Sleep 500ms
