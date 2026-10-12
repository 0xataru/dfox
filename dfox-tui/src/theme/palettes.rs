//! The built-in themes, one hex colour per token in `TOKENS` order:
//! bg, surface, surface_alt, border, border_focus, text, text_dim, accent,
//! accent_alt, success, warning, danger, null, number, string, keyword,
//! comment, selection.

pub const BUILTIN: &[(&str, &[&str])] = &[
    (
        "dfox",
        &[
            "#16161e", "#1e1f2b", "#262838", "#3b3f55", "#ff9e3b", "#e6e6ef", "#8a8fa8", "#ff9e3b",
            "#7aa2f7", "#9ece6a", "#e0af68", "#f7768e", "#6c7086", "#ff9e64", "#9ece6a", "#bb9af7",
            "#565f89", "#33364a",
        ],
    ),
    (
        "tokyo-night",
        &[
            "#1a1b26", "#1f2335", "#24283b", "#3b4261", "#7aa2f7", "#c0caf5", "#737aa2", "#7aa2f7",
            "#bb9af7", "#9ece6a", "#e0af68", "#f7768e", "#565f89", "#ff9e64", "#9ece6a", "#bb9af7",
            "#565f89", "#283457",
        ],
    ),
    (
        "catppuccin-mocha",
        &[
            "#1e1e2e", "#181825", "#313244", "#45475a", "#cba6f7", "#cdd6f4", "#a6adc8", "#cba6f7",
            "#89b4fa", "#a6e3a1", "#f9e2af", "#f38ba8", "#6c7086", "#fab387", "#a6e3a1", "#cba6f7",
            "#7f849c", "#45475a",
        ],
    ),
    (
        "catppuccin-latte",
        &[
            "#eff1f5", "#e6e9ef", "#ccd0da", "#bcc0cc", "#8839ef", "#4c4f69", "#6c6f85", "#8839ef",
            "#1e66f5", "#40a02b", "#df8e1d", "#d20f39", "#9ca0b0", "#fe640b", "#40a02b", "#8839ef",
            "#8c8fa1", "#ccd0da",
        ],
    ),
    (
        "gruvbox",
        &[
            "#282828", "#32302f", "#3c3836", "#504945", "#fabd2f", "#ebdbb2", "#a89984", "#fabd2f",
            "#83a598", "#b8bb26", "#fe8019", "#fb4934", "#7c6f64", "#d3869b", "#b8bb26", "#fb4934",
            "#928374", "#504945",
        ],
    ),
    (
        "nord",
        &[
            "#2e3440", "#3b4252", "#434c5e", "#4c566a", "#88c0d0", "#eceff4", "#9aa5b8", "#88c0d0",
            "#81a1c1", "#a3be8c", "#ebcb8b", "#bf616a", "#616e88", "#b48ead", "#a3be8c", "#81a1c1",
            "#616e88", "#434c5e",
        ],
    ),
    (
        "high-contrast",
        &[
            "#000000", "#000000", "#1a1a1a", "#ffffff", "#ffff00", "#ffffff", "#d0d0d0", "#ffff00",
            "#00ffff", "#00ff00", "#ffaf00", "#ff5f5f", "#bcbcbc", "#00ffff", "#00ff00", "#ff87ff",
            "#bcbcbc", "#005fd7",
        ],
    ),
];
