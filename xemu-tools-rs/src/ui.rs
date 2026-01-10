use eframe::egui;
use crate::halo::{GameObject, PlayerInfo, GameTimeInfo, Spawn, Item, FogInfo, TeamScores, NetworkGameClient, InputData, ObserverCamera, PerformanceMetrics};
use tokio::sync::mpsc;
use std::collections::VecDeque;

#[derive(PartialEq, Default)]
enum Tab {
    #[default]
    General,
    Players,
    Objects,
    Spawns,
    Items,
    Input,
    Events,
    Performance,
    RawJson,
}

pub struct UiState {
    pub game_time: Option<GameTimeInfo>,
    pub objects: Vec<GameObject>,
    pub players: Vec<PlayerInfo>,
    pub spawns: Vec<Spawn>,
    pub items: Vec<Item>,
    pub fog: Option<FogInfo>,
    pub team_scores: Option<TeamScores>,
    pub network_client: Option<NetworkGameClient>,
    pub input: Option<InputData>,
    pub camera: Option<ObserverCamera>,
    pub performance: PerformanceMetrics,
    pub events: Vec<String>,
    pub raw_json: String,
}

pub struct MyApp {
    rx: mpsc::Receiver<UiState>,
    state: Option<UiState>,
    perf_history: VecDeque<PerformanceMetrics>,
    event_history: VecDeque<String>,
    current_tab: Tab,
}

impl MyApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, rx: mpsc::Receiver<UiState>) -> Self {
        Self {
            rx,
            state: None,
            perf_history: VecDeque::with_capacity(100),
            event_history: VecDeque::with_capacity(500),
            current_tab: Tab::General,
        }
    }

    fn ui_performance(&self, ui: &mut egui::Ui) {
        if let Some(state) = &self.state {
            ui.heading("Performance Metrics");
            ui.label(format!("Game Info Read: {:.2} ms", state.performance.game_info_time));
            ui.label(format!("Post Steps: {:.2} ms", state.performance.post_steps_ms));
            ui.label(format!("Total Loop: {:.2} ms", state.performance.loop_time));
            ui.label(format!("Memory Usage: {:.2} MB", state.performance.memory_mbytes));

            use egui_plot::{Line, Plot, PlotPoints};
            
            let read_points: PlotPoints = self.perf_history.iter().enumerate()
                .map(|(i, p)| [i as f64, p.game_info_time]).collect();
            let post_points: PlotPoints = self.perf_history.iter().enumerate()
                .map(|(i, p)| [i as f64, p.post_steps_ms]).collect();
            let total_points: PlotPoints = self.perf_history.iter().enumerate()
                .map(|(i, p)| [i as f64, p.loop_time]).collect();

            Plot::new("perf_plot")
                .view_aspect(2.0)
                .legend(egui_plot::Legend::default())
                .show(ui, |plot_ui| {
                    plot_ui.line(Line::new(read_points).name("Game Info (ms)"));
                    plot_ui.line(Line::new(post_points).name("Post Steps (ms)"));
                    plot_ui.line(Line::new(total_points).name("Total Loop (ms)"));
                });
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll for new state
        while let Ok(new_state) = self.rx.try_recv() {
            self.perf_history.push_back(new_state.performance.clone());
            if self.perf_history.len() > 100 {
                self.perf_history.pop_front();
            }
            
            for e in &new_state.events {
                self.event_history.push_back(e.clone());
            }
            if self.event_history.len() > 500 {
                let to_remove = self.event_history.len() - 500;
                self.event_history.drain(0..to_remove);
            }

            self.state = Some(new_state);
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.current_tab, Tab::General, "General");
                ui.selectable_value(&mut self.current_tab, Tab::Players, "Players");
                ui.selectable_value(&mut self.current_tab, Tab::Objects, "Objects");
                ui.selectable_value(&mut self.current_tab, Tab::Spawns, "Spawns");
                ui.selectable_value(&mut self.current_tab, Tab::Items, "Items");
                ui.selectable_value(&mut self.current_tab, Tab::Input, "Input");
                ui.selectable_value(&mut self.current_tab, Tab::Events, "Events");
                ui.selectable_value(&mut self.current_tab, Tab::Performance, "Performance");
                ui.selectable_value(&mut self.current_tab, Tab::RawJson, "Raw JSON");
            });
        });

        if let Some(state) = &self.state {
            egui::CentralPanel::default().show(ctx, |ui| {
                match self.current_tab {
                    Tab::General => {
                        if let Some(gt) = &state.game_time {
                            ui.heading("Game Time");
                            ui.label(format!("Time: {} | Speed: {:.2} | Paused: {}", gt.game_time, gt.game_time_speed, gt.game_time_paused));
                        }
                        ui.separator();
                        if let Some(scores) = &state.team_scores {
                            ui.heading("Scores");
                            ui.label(format!("Slayer: {:?} / {}", scores.slayer_team_score, scores.slayer_score_limit));
                            ui.label(format!("CTF: {:?} / {}", scores.ctf_team_score, scores.ctf_score_limit));
                        }
                        ui.separator();
                        if let Some(fog) = &state.fog {
                             ui.heading("Fog");
                             ui.label(format!("Density: {:.2}", fog.max_density));
                             ui.label(format!("Color: {:.2} {:.2} {:.2}", fog.color_r, fog.color_g, fog.color_b));
                        }
                        ui.separator();
                        if let Some(net) = &state.network_client {
                            ui.heading("Network Client");
                            ui.label(format!("Ping: {}", net.average_ping));
                            ui.label(format!("Sent/Recv: {} / {}", net.packets_sent, net.packets_received));
                        }
                        ui.separator();
                        if let Some(cam) = &state.camera {
                            ui.heading("Camera");
                            ui.label(format!("Pos: {:.2}, {:.2}, {:.2}", cam.x, cam.y, cam.z));
                            ui.label(format!("Aim: {:.2}, {:.2}, {:.2}", cam.x_aim, cam.y_aim, cam.z_aim));
                            ui.label(format!("FOV: {:.2}", cam.fov));
                        }
                    }
                    Tab::Players => {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                             for player in &state.players {
                                ui.group(|ui| {
                                    ui.heading(format!("[{}] {}", player.player_index, player.name));
                                    ui.label(format!("Team: {} | Color: {}", player.team, player.color));
                                    ui.label(format!("Machine: {} | Controller: {}", player.machine_index, player.controller_index));
                                    if let Some(obj) = &player.player_object_data {
                                        ui.label(format!("Pos: {:.2}, {:.2}, {:.2}", obj.x, obj.y, obj.z));
                                        ui.label(format!("Health: {:.2}", obj.health));
                                        ui.label(format!("Shields: {:.2}", obj.shields));
                                        if let Some(w) = obj.weapons.first() {
                                            ui.label(format!("Weapon: {}", w.tag_name));
                                            ui.label(format!("Ammo: {}", w.magazine_ammo_count));
                                        }
                                    } else {
                                        ui.label("No Player Object (Dead/Respawning)");
                                    }
                                    if !player.damage_table.is_empty() {
                                        ui.collapsing("Recent Damage", |ui| {
                                            for dmg in &player.damage_table {
                                                ui.label(format!("- {:.2} dmg @ tick {}", dmg.damage_amount, dmg.damage_time));
                                            }
                                        });
                                    }
                                });
                            }
                        });
                    }
                    Tab::Objects => {
                        ui.label(format!("Count: {}", state.objects.len()));
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for obj in state.objects.iter().take(50) {
                                ui.group(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(format!("[{}]", obj.object_id));
                                        ui.label(format!("{}", obj.object_type_string));
                                    });
                                    ui.label(format!("Pos: {:.2}, {:.2}, {:.2}", obj.x, obj.y, obj.z));
                                });
                            }
                            if state.objects.len() > 50 {
                                ui.label(format!("... {} more objects hidden ...", state.objects.len() - 50));
                            }
                        });
                    }
                    Tab::Spawns => {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for spawn in &state.spawns {
                                ui.label(format!("[{}] Team: {} | Pos: {:.2}, {:.2}, {:.2}",
                                    spawn.spawn_id, spawn.team_index, spawn.x, spawn.y, spawn.z));
                            }
                        });
                    }
                    Tab::Items => {
                         egui::ScrollArea::vertical().show(ui, |ui| {
                            for item in &state.items {
                                ui.label(format!("Tag: {} | Pos: {:.2}, {:.2}, {:.2}", item.tag_name, item.x, item.y, item.z));
                            }
                        });
                    }
                    Tab::Input => {
                        if let Some(input) = &state.input {
                            ui.heading(format!("Local Player {}", input.local_player_index));
                            ui.label(format!("A: {}", input.buttons.a));
                            ui.label(format!("B: {}", input.buttons.b));
                            ui.label(format!("X: {}", input.buttons.x));
                            ui.label(format!("Y: {}", input.buttons.y));
                        } else {
                            ui.label("No Input Data");
                        }
                    }
                    Tab::Events => {
                        egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                            for e in &self.event_history {
                                ui.label(e);
                            }
                        });
                    }
                    Tab::Performance => {
                        self.ui_performance(ui);
                    }
                    Tab::RawJson => {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let text_style = egui::TextStyle::Monospace;
                            let mut json = state.raw_json.clone();
                            ui.add(
                                egui::TextEdit::multiline(&mut json)
                                    .font(text_style)
                                    .code_editor()
                                    .desired_width(f32::INFINITY)
                            );
                        });
                    }
                }
            });
        }
        
        ctx.request_repaint();
    }
}
