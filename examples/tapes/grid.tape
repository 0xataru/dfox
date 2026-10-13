# The result grid: 50,000 rows streamed in, a cell cursor, range selection,
# resizing a column and copying as JSON.
Output examples/demo/grid.gif
Source examples/tapes/common.tape

Hide
Type "clear" Enter
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
Shift+Down@120ms 4
Shift+Right@120ms 1
Sleep 800ms
Type ">>"
Sleep 600ms
Type "y"
Sleep 800ms
Type "j"
Sleep 1.5s
Ctrl+Q
Type "y"
Sleep 500ms
