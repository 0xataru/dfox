# Shared settings for every tape: `Source examples/tapes/common.tape`.
Require sqlite3
Set Shell "bash"
Set FontSize 15
Set Width 1400
Set Height 820
Set Padding 16
Set TypingSpeed 40ms
Set Framerate 30
Env DFOX_CONFIG_DIR "target/demo/config"
Env DFOX_STATE_DIR "target/demo/state"
Env DATABASE_URL ""
Env PGPASSFILE "/nonexistent"
