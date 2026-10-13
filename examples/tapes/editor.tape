# The editor: several statements, running the one under the cursor, query
# tabs, history search and the guard on destructive statements.
Output examples/demo/editor.gif
Source examples/tapes/common.tape

Hide
Type "clear" Enter
Show

Type "./target/release/dfox-tui --reduced-motion" Enter
Sleep 1s
Enter
Sleep 1s

Type "select count(*) as customers from customers;" Enter
Type "select * from big_spenders;"
Sleep 400ms
# Runs the statement under the cursor; the bar in the margin marks it.
Ctrl+E
Sleep 1.5s
Up
Ctrl+E
Sleep 1.5s

# A second tab, and the history of the first.
Ctrl+N
Sleep 500ms
Ctrl+R
Sleep 500ms
Type "spend"
Sleep 800ms
Enter
Sleep 800ms

# DELETE without WHERE asks first.
Ctrl+A
Type "delete from orders"
Ctrl+E
Sleep 2s
Type "n"
Sleep 800ms
Ctrl+Q
Type "y"
Sleep 500ms
