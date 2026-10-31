//! Agents using the mouse and keys in the windows, while the user lets
//! them: see `crate::agent_hands`.

use eframe::egui;
use musicable_mcp::{Hands, Reply};
use serde_json::json;

use super::App;
use crate::ui_tree::Ask;

impl App {
    /// Has the command log say the agent using the mouse and keys ran what
    /// runs from now on, while one is, and the user otherwise: all the
    /// input is the agent's while it is, as the user's own stops it.
    pub(super) fn log_hands(&mut self, ctx: &egui::Context) {
        let agent = crate::agent_hands::agent(ctx);
        self.session.command_log_mut().set_by(agent);
    }

    /// Starts what `hands` says to do with the mouse and keys, and answers
    /// `reply` once it's done, or says why it can't be.
    pub(super) fn hands_for_agent(&mut self, hands: Hands, reply: Reply, ctx: &egui::Context) {
        if !self.settings.agents.mouse_and_keys {
            return reply.failed(
                "The user doesn't let agents use the mouse and keys: the switch for it, Let \
                 agents use the mouse and keys, is on the Agents page of Settings, and only they \
                 can turn it on. run_command does what the commands do without them.",
            );
        }
        let (window, viewport, _) = match self.place(hands.window, None) {
            Ok(place) => place,
            Err(why) => return reply.failed(why),
        };
        let minimized = ctx.input_for(viewport, |input| input.viewport().minimized);
        if minimized == Some(true) {
            return reply.failed("The window is minimized, so it can't be used.");
        }
        let watch = match hands.watch {
            Some(watch) => {
                let panel = match watch.panel {
                    Some(panel) => match self.place(None, Some(&panel)) {
                        Ok((in_window, _, Some(rect))) if in_window == window => {
                            Some((panel, rect))
                        }
                        Ok(_) => {
                            return reply.failed(format!(
                                "The panel {panel} isn't in that window: get_windows says \
                                 where the panels are."
                            ));
                        }
                        Err(why) => return reply.failed(why),
                    },
                    None => None,
                };
                Some(Ask {
                    window: window.map_or(json!("main"), |index| json!(index + 1)),
                    panel,
                    filter: watch.filter,
                    limit: watch.limit,
                })
            }
            None => None,
        };
        crate::agent_hands::start(ctx, viewport, hands.steps, watch, reply);
    }
}
