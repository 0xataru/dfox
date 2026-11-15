//! Which Kubernetes contexts' databases the connection list shows, picked
//! from the kubeconfig's: `kube_contexts` in the config, set from the app.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::panel;
use crate::{component::RenderCtx, connections::kubernetes::short_context, widgets::centered};

/// One line of the picker: the current context, whatever it is, or one by
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    /// `None` for "the current context".
    context: Option<String>,
    on: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterPicker {
    rows: Vec<Row>,
    current: Option<String>,
    selected: usize,
}

fn is_prod(context: &str) -> bool {
    context.to_lowercase().contains("prod")
}

impl ClusterPicker {
    /// The kubeconfig's `contexts` and its `current` one, ticked as `wanted`
    /// (`kube_contexts`) asks: `all` ticks every context but prod ones.
    pub fn new(contexts: Vec<String>, current: Option<String>, wanted: &[String]) -> Self {
        let all = wanted.iter().any(|w| w == "all");
        let mut rows = vec![Row {
            context: None,
            on: wanted.iter().any(|w| w == "current"),
        }];
        rows.extend(contexts.into_iter().map(|context| Row {
            on: wanted.contains(&context) || (all && !is_prod(&context)),
            context: Some(context),
        }));
        Self {
            rows,
            current,
            selected: 0,
        }
    }

    /// Reads the contexts from the local kubeconfig, without asking any
    /// cluster.
    pub fn open(wanted: &[String]) -> Result<Self, String> {
        let run = |args: &[&str]| {
            std::process::Command::new("kubectl")
                .args(args)
                .output()
                .map_err(|err| match err.kind() {
                    std::io::ErrorKind::NotFound => {
                        dfox_core::models::connections::KUBECTL_MISSING.to_string()
                    }
                    _ => format!("kubectl: {err}"),
                })
        };
        let names = run(&["config", "get-contexts", "-o", "name"])?;
        let contexts: Vec<String> = String::from_utf8_lossy(&names.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        if contexts.is_empty() {
            return Err("kubectl knows no contexts: the kubeconfig has no clusters".into());
        }
        let current = run(&["config", "current-context"])
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
        Ok(Self::new(contexts, current, wanted))
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn down(&mut self) {
        self.selected = (self.selected + 1).min(self.rows.len() - 1);
    }

    pub fn toggle(&mut self) {
        let row = &mut self.rows[self.selected];
        row.on = !row.on;
    }

    /// `kube_contexts` as ticked.
    pub fn chosen(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|r| r.on)
            .map(|r| r.context.clone().unwrap_or_else(|| "current".to_string()))
            .collect()
    }

    /// A prod cluster ticked, asked every minute once saved.
    fn prod_ticked(&self) -> bool {
        self.rows
            .iter()
            .any(|r| r.on && r.context.as_deref().is_some_and(is_prod))
    }

    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let mut lines = vec![Line::from("")];
        for (i, row) in self.rows.iter().enumerate() {
            let marker = if i == self.selected {
                ctx.icons.selected
            } else {
                " "
            };
            let tick = if row.on { "[x]" } else { "[ ]" };
            let mut spans = vec![Span::styled(format!("{marker} {tick} "), theme.key())];
            match &row.context {
                None => {
                    spans.push(Span::raw("the current context"));
                    if let Some(current) = &self.current {
                        spans.push(Span::styled(
                            format!("  {}", short_context(current)),
                            theme.dim(),
                        ));
                    }
                }
                Some(context) => {
                    spans.push(Span::raw(short_context(context)));
                    if is_prod(context) {
                        spans.push(Span::styled("  prod", theme.danger()));
                    }
                }
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(""));
        if self.prod_ticked() {
            lines.push(Line::styled(
                "A prod cluster is asked for its services every minute",
                theme.danger(),
            ));
        }
        lines.push(Line::styled(
            "Space ticks · Enter saves · Esc cancels",
            theme.dim(),
        ));
        let height = lines.len() as u16 + 2;
        let popup = centered(area, 60, height);
        f.render_widget(Clear, popup);
        f.render_widget(
            Paragraph::new(lines).block(panel(vec![Span::raw("Kubernetes clusters")], true, ctx)),
            popup,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, TestCtx};

    fn contexts() -> Vec<String> {
        [
            "do-ams3-k8s-1-29-stage-crm-1-do-0-ams3-1714463690478",
            "gke_emerald-griffin-400808_europe-west1-b_profi-crm-stage-cluster",
            "gke_profi-crm-416111_europe-west1-b_product-crm-prod",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn ticks_follow_the_config_and_save_back() {
        let current = Some(contexts()[1].clone());
        let mut picker = ClusterPicker::new(contexts(), current.clone(), &["current".into()]);
        assert_eq!(picker.chosen(), ["current"]);
        // The DigitalOcean stage too.
        picker.down();
        picker.toggle();
        assert_eq!(
            picker.chosen(),
            ["current".to_string(), contexts()[0].clone()]
        );
        // `all` ticks every cluster but prod.
        let all = ClusterPicker::new(contexts(), current, &["all".into()]);
        assert_eq!(all.chosen(), contexts()[..2]);
        assert!(!all.prod_ticked());
    }

    #[test]
    fn picker() {
        let test = TestCtx::new();
        let mut picker =
            ClusterPicker::new(contexts(), Some(contexts()[1].clone()), &["current".into()]);
        picker.down();
        picker.down();
        picker.down();
        picker.toggle();
        insta::assert_snapshot!(render(&test, 70, 12, |f, area, ctx| picker.render(f, area, ctx)));
    }
}
