# The tour: splash, connections, explorer, a query with autocomplete, the
# grid and the record inspector, the command palette.
Output examples/demo/overview.gif
Source examples/tapes/common.tape

# Each tape starts with an empty history.
Hide
Type "rm -f target/demo/state/history.jsonl && clear" Enter
Show

Type "./target/release/dfox" Sleep 300ms Enter
Sleep 2s

# Connect to the saved shop database.
Enter
Sleep 1.5s

# Autocomplete: a table after FROM, columns after the alias.
Type "select c.name, c.country, o.quantity from cust"
Sleep 700ms
Tab
Type " c join ord"
Sleep 500ms
Tab
Type " o on o.customer_id = c."
Sleep 700ms
Tab
Sleep 500ms
Ctrl+E
Sleep 1.5s

# The results, then the inspector for one row.
Tab
Down@150ms 4
Enter
Sleep 2s
Escape
Sleep 500ms

# The command palette.
Ctrl+P
Sleep 400ms
Type "theme"
Sleep 600ms
Enter
Sleep 1.5s
# Quit off camera, so the GIF ends on dfox, not the shell.
Hide
Ctrl+Q
Type "y"
Sleep 500ms
