use crate::config::Config;
use crate::events::{ExtractResult, GameMeta};
use crate::memory::MemoryReader;
use crate::util::{
    datetime_minus_ticks, f32_value, f64_value, py_datetime_string, py_hex_i32, py_hex_u32,
    py_hex_u64, timedelta_seconds_floor,
};
use anyhow::Result;
use chrono::{DateTime, Local};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
struct HaloGlobals {
    player_datum_array: u32,
    player_datum_array_element_size: u16,
    player_datum_array_first_element_address: u32,
    players_globals_address: u32,
    game_globals_address: u32,
    game_connection_address: u32,
    game_time_globals_address: u32,
    global_tag_instances_address: u32,
    game_time_address: u32,
}

pub struct HaloReader {
    pub mem: MemoryReader,
    config: Config,
    globals: HaloGlobals,
    spawns_cache: Vec<Value>,
    items_cache: Vec<Value>,
    last_game_connection: Value,
    last_game_in_progress: (u8, u8, u8),
    pub game_meta: GameMeta,
    object_type_string_cache: HashMap<u8, String>,
}

impl HaloReader {
    pub fn new(mut mem: MemoryReader, config: Config) -> Result<Self> {
        let player_datum_array = mem.read_u32(0x2FAD28)?;
        let _player_datum_array_max_count = mem.read_u16(player_datum_array as u64 + 0x20)?;
        let player_datum_array_element_size = mem.read_u16(player_datum_array as u64 + 0x22)?;
        let player_datum_array_first_element_address =
            mem.read_u32(player_datum_array as u64 + 0x34)?;

        let players_globals_address = mem.read_u32(0x2FAD20)?;
        let _teams_address = mem.read_u32(0x2FAD24)?;
        let game_globals_address = mem.read_u32(0x27629C)?;
        let _global_game_globals_address = mem.read_u32(0x39BE4C)?;
        let _game_server_address = mem.read_u32(0x2E3628)?;
        let _game_client_address = mem.read_u32(0x2E362C)?;
        let game_connection_address = 0x2E3684;
        let _is_team_game_address = mem.read_u8(0x2F90C4)?;
        let game_time_globals_address = mem.read_u32(0x2F8CA0)?;
        let global_tag_instances_address = mem.read_u32(0x39CE24)?;
        let _hud_messages_pointer = mem.read_u32(0x276B40)?;
        let _something_saying_main_menu = mem.read_u32(0x2E4000 + 4)?;
        let game_time_address = game_time_globals_address + 12;

        Ok(Self {
            mem,
            config,
            globals: HaloGlobals {
                player_datum_array,
                player_datum_array_element_size,
                player_datum_array_first_element_address,
                players_globals_address,
                game_globals_address,
                game_connection_address,
                game_time_globals_address,
                global_tag_instances_address,
                game_time_address,
            },
            spawns_cache: Vec::new(),
            items_cache: Vec::new(),
            last_game_connection: Value::String(String::new()),
            last_game_in_progress: (0, 0, 0),
            game_meta: GameMeta::default(),
            object_type_string_cache: HashMap::new(),
        })
    }

    pub fn game_time_host_address(&mut self) -> Result<u64> {
        self.mem.get_host_address(self.globals.game_time_address as u64)
    }

    pub fn read_loop_game_time(&mut self) -> Result<i64> {
        Ok(self.mem.read_u32(self.globals.game_time_address as u64)? as i64 - 1)
    }

    pub fn populate_memory_cache(&mut self) -> Result<()> {
        self.mem.populate_cache()
    }

    pub fn invalidate_memory_cache(&mut self) {
        self.mem.invalidate_cache();
    }

    pub fn clear_caches(&mut self) {
        self.spawns_cache.clear();
        self.items_cache.clear();
    }

    pub fn get_game_info(&mut self) -> Result<Value> {
        let player_count = self.mem.read_u16(self.globals.player_datum_array as u64 + 0x2E)?;
        let mut player_stat_array = Vec::new();
        let mut damage_counts = Map::new();

        let game_time_raw = self.mem.read_u32(self.globals.game_time_globals_address as u64 + 12)?;
        let _game_time_elapsed = self.mem.read_u32(self.globals.game_time_globals_address as u64 + 16)?;
        let game_time_initialized = self.mem.read_u8(self.globals.game_time_globals_address as u64)?;
        let game_time_active = self.mem.read_u8(self.globals.game_time_globals_address as u64 + 1)?;
        let game_time_paused = self.mem.read_u8(self.globals.game_time_globals_address as u64 + 2)?;
        let _game_time_speed = self.mem.read_f32(self.globals.game_time_globals_address as u64 + 24)?;
        let _game_time_leftover_dt =
            self.mem.read_f32(self.globals.game_time_globals_address as u64 + 28)?;

        let game_globals_map_loaded = self.mem.read_u8(self.globals.game_globals_address as u64)?;
        let game_globals_active = self.mem.read_u8(self.globals.game_globals_address as u64 + 1)?;
        let main_menu_is_active = self.mem.read_u8(0x2E4068)?;
        let game_engine_globals_address = self.mem.read_u32(0x2F9110)?;

        let new_game_in_progress = (game_time_initialized, game_time_active, game_time_paused);
        if self.last_game_in_progress != new_game_in_progress {
            println!(
                "game in progress changed to game_time_initialized={} game_time_active={} game_time_paused={}",
                game_time_initialized, game_time_active, game_time_paused
            );
            self.last_game_in_progress = new_game_in_progress;
        }

        let game_connection = self.mem.read_u16(self.globals.game_connection_address as u64)?;
        if self.last_game_connection != json!(game_connection) {
            println!("game_connection changed to {}", py_hex_u32(game_connection as u32));
            self.last_game_connection = json!(game_connection);
        }

        let object_header_datum_array = self.mem.read_u32(0x2FC6AC)?;
        let object_header_datum_array_element_size =
            self.mem.read_u16(object_header_datum_array as u64 + 0x22)?;
        let object_header_datum_array_first_element_address =
            self.mem.read_u32(object_header_datum_array as u64 + 0x34)?;

        if game_time_initialized != 0 && game_time_active != 0 && main_menu_is_active == 0 {
            for player_index in 0..player_count {
                let static_player_address = self.globals.player_datum_array_first_element_address as u64
                    + player_index as u64 * self.globals.player_datum_array_element_size as u64;
                let player_object_handle = self.mem.read_i32(static_player_address + 0x34)?;
                let previous_player_object_handle = self.mem.read_i32(static_player_address + 0x38)?;
                let player_object_id = (player_object_handle as u32) & 0xFFFF;

                let dynamic_player_address = self.mem.read_u32(
                    object_header_datum_array_first_element_address as u64
                        + ((player_object_handle as u32) & 0xFFFF) as u64
                            * object_header_datum_array_element_size as u64
                        + 8,
                )?;
                let previous_dynamic_player_address = self.mem.read_u32(
                    object_header_datum_array_first_element_address as u64
                        + ((previous_player_object_handle as u32) & 0xFFFF) as u64
                            * object_header_datum_array_element_size as u64
                        + 8,
                )?;

                let mut player_object_debug = Map::new();
                player_object_debug.insert(
                    "player_object_handle".to_string(),
                    Value::String(py_hex_i32(player_object_handle)),
                );
                let object_header_host = self.mem.known_host_address(object_header_datum_array as u64)
                    .or_else(|_| self.mem.get_host_address(object_header_datum_array as u64))?;
                player_object_debug.insert(
                    "object_header_datum_array".to_string(),
                    Value::String(format!(
                        "{} @ {} -> {}",
                        py_hex_u32(self.mem.read_u32(object_header_datum_array as u64)?),
                        py_hex_u32(object_header_datum_array),
                        py_hex_u64(object_header_host)
                    )),
                );
                player_object_debug.insert(
                    "object_header_datum_array_first_element_address".to_string(),
                    Value::String(py_hex_u32(object_header_datum_array_first_element_address)),
                );
                player_object_debug.insert(
                    "dynamic_player_address".to_string(),
                    if player_object_handle != -1 {
                        Value::String(format!(
                            "{} -> {}",
                            py_hex_u32(dynamic_player_address),
                            py_hex_u64(self.mem.get_host_address(dynamic_player_address as u64)?)
                        ))
                    } else {
                        Value::String(String::new())
                    },
                );
                player_object_debug.insert("player_object_id".to_string(), json!(player_object_id));
                player_object_debug.insert(
                    "static_player_address".to_string(),
                    Value::String(format!(
                        "{} -> {}",
                        py_hex_u64(static_player_address),
                        py_hex_u64(self.mem.get_host_address(static_player_address)?)
                    )),
                );

                let damage_table_address = if player_object_handle == -1 {
                    self.mem.read_u32(
                        object_header_datum_array_first_element_address as u64
                            + ((previous_player_object_handle as u32) & 0xFFFF) as u64
                                * object_header_datum_array_element_size as u64
                            + 8,
                    )? as u64
                        + 0x3E0
                } else {
                    dynamic_player_address as u64 + 0x3E0
                };
                player_object_debug.insert(
                    "damage_table_address".to_string(),
                    Value::String(format!(
                        "{} -> {}",
                        py_hex_u64(damage_table_address),
                        py_hex_u64(self.mem.get_host_address(damage_table_address)?)
                    )),
                );

                let mut damage_table = Vec::new();
                for i in 0..4u64 {
                    let entry_address = damage_table_address + 16 * i;
                    let damage_time = self.mem.read_u32(entry_address)?;
                    if damage_time != 0xFFFF_FFFF {
                        let damage_amount = self.mem.read_f32(entry_address + 4)?;
                        let dynamic_player = self.mem.read_u32(entry_address + 8)?;
                        let static_player = self.mem.read_u32(entry_address + 12)?;
                        let mut entry = Map::new();
                        entry.insert("damage_time".to_string(), json!(damage_time));
                        entry.insert("damage_amount".to_string(), f32_value(damage_amount));
                        entry.insert("dynamic_player".to_string(), json!(dynamic_player));
                        entry.insert("static_player".to_string(), json!(static_player));
                        entry.insert(
                            "dynamic_player_hex".to_string(),
                            Value::String(py_hex_u32(dynamic_player)),
                        );
                        entry.insert(
                            "static_player_hex".to_string(),
                            Value::String(py_hex_u32(static_player)),
                        );
                        damage_table.push(Value::Object(entry));

                        let last_death = self.mem.read_u32(static_player_address + 0x84)?;
                        if player_object_handle != -1 || last_death == game_time_raw.saturating_sub(1) {
                            let dealer = (static_player & 0xFFFF).to_string();
                            let receiver = player_index.to_string();
                            let dealer_map = damage_counts
                                .entry(dealer)
                                .or_insert_with(|| Value::Object(Map::new()));
                            if let Some(map) = dealer_map.as_object_mut() {
                                map.insert(receiver, f32_value(damage_amount));
                            }
                        }
                    }
                }

                let (player_object_data, model_nodes) = if player_object_handle != -1 {
                    let player_object_data = self.get_player_object_data(
                        dynamic_player_address as u64,
                        object_header_datum_array,
                        &mut player_object_debug,
                    )?;
                    let model_nodes = self.get_model_nodes(dynamic_player_address as u64)?;
                    (player_object_data, model_nodes)
                } else {
                    let model_nodes = if previous_player_object_handle != -1 {
                        self.get_model_nodes(previous_dynamic_player_address as u64)?
                    } else {
                        Vec::new()
                    };
                    (Value::Object(Map::new()), model_nodes)
                };

                let local_player = self.mem.read_i16(static_player_address + 0x2)?;
                let player_name = self.read_utf16_string(static_player_address + 0x4, 24)?;
                let gametype = if game_engine_globals_address != 0 {
                    self.mem.read_u32(game_engine_globals_address as u64 + 0x4)?
                } else {
                    0
                };

                let mut player_stats = Map::new();
                player_stats.insert("player_index".to_string(), json!(player_index));
                player_stats.insert("local_player".to_string(), json!(local_player));
                player_stats.insert("name".to_string(), Value::String(player_name));
                player_stats.insert("team".to_string(), json!(self.mem.read_u32(static_player_address + 0x20)?));
                player_stats.insert(
                    "action_target".to_string(),
                    Value::String(py_hex_u32(self.mem.read_u32(static_player_address + 0x24)?)),
                );
                player_stats.insert("action".to_string(), json!(self.mem.read_u16(static_player_address + 0x28)?));
                player_stats.insert("action_seat".to_string(), json!(self.mem.read_u16(static_player_address + 0x2A)?));
                player_stats.insert("respawn_timer".to_string(), json!(self.mem.read_u32(static_player_address + 0x2C)?));
                player_stats.insert("respawn_penalty".to_string(), json!(self.mem.read_u32(static_player_address + 0x30)?));
                player_stats.insert(
                    "object_ref".to_string(),
                    Value::String(py_hex_u32(self.mem.read_u32(static_player_address + 0x34)?)),
                );
                player_stats.insert("object_index".to_string(), json!(self.mem.read_u16(static_player_address + 0x34)?));
                player_stats.insert("object_id".to_string(), json!(self.mem.read_u16(static_player_address + 0x36)?));
                player_stats.insert(
                    "previous_object_ref".to_string(),
                    Value::String(py_hex_u32(self.mem.read_u32(static_player_address + 0x38)?)),
                );
                player_stats.insert(
                    "last_target_object_ref".to_string(),
                    Value::String(py_hex_u32(self.mem.read_u32(static_player_address + 0x40)?)),
                );
                player_stats.insert("time_of_last_shot".to_string(), json!(self.mem.read_u32(static_player_address + 0x44)?));
                player_stats.insert("player_speed".to_string(), f32_value(self.mem.read_f32(static_player_address + 0x6C)?));
                player_stats.insert("camo_timer".to_string(), json!(self.mem.read_u32(static_player_address + 0x68)?));
                player_stats.insert("time_of_last_death".to_string(), json!(self.mem.read_u32(static_player_address + 0x84)?));
                player_stats.insert("target_player_index".to_string(), json!(self.mem.read_u32(static_player_address + 0x88)?));
                player_stats.insert("kill_streak".to_string(), json!(self.mem.read_u16(static_player_address + 0x92)?));
                player_stats.insert("multikill".to_string(), json!(self.mem.read_u16(static_player_address + 0x94)?));
                player_stats.insert("time_of_last_kill".to_string(), json!(self.mem.read_i16(static_player_address + 0x96)?));
                player_stats.insert("kills".to_string(), json!(self.mem.read_i16(static_player_address + 0x98)?));
                player_stats.insert("assists".to_string(), json!(self.mem.read_i16(static_player_address + 0xA0)?));
                player_stats.insert("team_kills".to_string(), json!(self.mem.read_i16(static_player_address + 0xA8)?));
                player_stats.insert("deaths".to_string(), json!(self.mem.read_i16(static_player_address + 0xAA)?));
                player_stats.insert("suicides".to_string(), json!(self.mem.read_i16(static_player_address + 0xAC)?));
                player_stats.insert("shots_fired".to_string(), json!(self.mem.read_i32(static_player_address + 0xAE)?));
                player_stats.insert("shots_hit".to_string(), json!(self.mem.read_i16(static_player_address + 0xB2)?));
                player_stats.insert(
                    "score".to_string(),
                    json!(self.player_score_by_player_id(player_index as u32, gametype)?),
                );
                player_stats.insert("ctf_score".to_string(), json!(self.mem.read_i16(static_player_address + 0xC4)?));
                player_stats.insert("player_quit".to_string(), json!(self.mem.read_u8(static_player_address + 0xD1)?));
                player_stats.insert("damage_table".to_string(), Value::Array(damage_table));
                player_stats.insert(
                    "observer_camera_info".to_string(),
                    self.get_observer_camera_info(local_player)?,
                );
                player_stats.insert(
                    "input_data".to_string(),
                    self.get_input_data(local_player, player_index as u32)?,
                );
                player_stats.insert("player_object_debug".to_string(), Value::Object(player_object_debug));
                player_stats.insert("player_object_data".to_string(), player_object_data.clone());
                player_stats.insert("model_nodes".to_string(), Value::Array(model_nodes));

                let has_camo = player_object_data
                    .get("camo")
                    .and_then(Value::as_u64)
                    .map(|value| value == 0x51)
                    .unwrap_or(false);
                let has_overshield = player_object_data
                    .as_object()
                    .map(|object| {
                        object.get("shields_status").and_then(Value::as_u64) == Some(0x10)
                            || object.get("shields").and_then(Value::as_f64).unwrap_or(0.0) > 1.0
                    })
                    .unwrap_or(false);
                player_stats.insert(
                    "derived_stats".to_string(),
                    json!({
                        "has_camo": has_camo,
                        "has_overshield": has_overshield,
                    }),
                );

                if local_player != -1 {
                    player_stats.insert(
                        "first_person_weapon".to_string(),
                        self.get_first_person_weapon(local_player)?,
                    );
                }

                player_stat_array.push(Value::Object(player_stats));
            }
        }

        let current_time: DateTime<Local> = Local::now();
        let game_time_info = self.get_game_time_info()?;
        let elapsed_time = game_time_info
            .get("game_time")
            .and_then(Value::as_i64)
            .unwrap_or(-1)
            + 1;
        let start_time = datetime_minus_ticks(current_time, elapsed_time.max(0) as u32);
        let current_time_string = py_datetime_string(current_time);
        let start_time_string = py_datetime_string(start_time);

        let game_type_value = if game_engine_globals_address != 0 {
            json!(self.mem.read_u32(game_engine_globals_address as u64 + 0x4)?)
        } else {
            Value::String(String::new())
        };
        let game_engine_running = game_engine_globals_address != 0;
        let game_engine_can_score = self.mem.read_u32(0x2FABF0)? == 0 && game_engine_globals_address != 0;

        let mut game_info = Map::new();
        game_info.insert(
            "process_id".to_string(),
            Value::String(format!("{} - {:#x}", self.mem.pid, self.mem.pid)),
        );
        game_info.insert("game_type".to_string(), game_type_value);
        game_info.insert("variant".to_string(), json!(self.mem.read_u8(0x2F90F4)?));
        game_info.insert("gametype_settings".to_string(), self.get_global_variant()?);
        game_info.insert(
            "global_stage".to_string(),
            Value::String(self.mem.read_string(0x2FAC20, 63)?),
        );
        game_info.insert(
            "multiplayer_map_name".to_string(),
            Value::String(self.mem.read_string(0x2E37CD, 128)?),
        );
        game_info.insert("map_info".to_string(), self.get_map_info()?);
        game_info.insert("game_connection".to_string(), json!(self.mem.read_i16(0x2E3684)?));
        game_info.insert("game_engine_has_teams".to_string(), json!(self.mem.read_u8(0x2F90C4)?));
        game_info.insert("game_engine_running".to_string(), json!(game_engine_running));
        game_info.insert("game_engine_can_score".to_string(), json!(game_engine_can_score));
        game_info.insert("flag_data".to_string(), self.get_flag_data()?);
        game_info.insert(
            "local_player_count".to_string(),
            json!(self.mem.read_u16(self.globals.players_globals_address as u64 + 0x24)?),
        );
        game_info.insert("game_time_info".to_string(), game_time_info);
        game_info.insert("network_game_server".to_string(), self.get_network_game_server()?);
        game_info.insert("network_game_client".to_string(), self.get_network_game_client()?);
        game_info.insert(
            "observer_cameras_address".to_string(),
            Value::String(format!("{:#x}", self.mem.get_host_address(0x271550)?)),
        );
        game_info.insert(
            "game_globals_address".to_string(),
            Value::String(format!(
                "{} -> {}",
                py_hex_u32(self.globals.game_globals_address),
                py_hex_u64(self.mem.get_host_address(self.globals.game_globals_address as u64)?)
            )),
        );
        game_info.insert("game_globals_map_loaded".to_string(), json!(game_globals_map_loaded));
        game_info.insert(
            "players_are_double_speed".to_string(),
            json!(self.mem.read_u8(self.globals.game_globals_address as u64 + 0x2)?),
        );
        game_info.insert(
            "game_loading_in_progress".to_string(),
            json!(self.mem.read_u8(self.globals.game_globals_address as u64 + 0x3)?),
        );
        game_info.insert(
            "precache_map_status".to_string(),
            f32_value(self.mem.read_f32(self.globals.game_globals_address as u64 + 0x4)?),
        );
        game_info.insert(
            "game_difficulty_level".to_string(),
            json!(self.mem.read_u8(self.globals.game_globals_address as u64 + 0xE)?),
        );
        game_info.insert("game_globals_active".to_string(), json!(game_globals_active));
        game_info.insert(
            "global_random_seed".to_string(),
            Value::String(py_hex_u32(self.mem.read_u32(0x2E3648)?)),
        );
        game_info.insert(
            "stored_global_random".to_string(),
            Value::String(py_hex_u32(
                self.mem.read_u32(self.globals.game_globals_address as u64 + 16)?,
            )),
        );
        game_info.insert("main_menu_is_active".to_string(), json!(main_menu_is_active));
        game_info.insert(
            "last_game_in_progress".to_string(),
            json!([
                self.last_game_in_progress.0,
                self.last_game_in_progress.1,
                self.last_game_in_progress.2
            ]),
        );
        game_info.insert("last_game_connection".to_string(), self.last_game_connection.clone());
        game_info.insert("memory_info".to_string(), self.get_memory_info()?);
        game_info.insert("events".to_string(), Value::Array(Vec::new()));
        game_info.insert("damage_counts".to_string(), Value::Object(damage_counts));
        game_info.insert("players".to_string(), Value::Array(player_stat_array));
        let objects = self.get_objects()?;
        game_info.insert("objects".to_string(), Value::Array(objects.clone()));
        game_info.insert("items".to_string(), Value::Array(self.get_items(true)?));
        game_info.insert("spawns".to_string(), Value::Array(self.get_spawns(true)?));
        game_info.insert("game_ended_this_tick".to_string(), Value::Bool(false));
        game_info.insert("current_time".to_string(), Value::String(current_time_string));
        game_info.insert("start_time".to_string(), Value::String(start_time_string.clone()));
        game_info.insert("objects_meta".to_string(), self.arrange_objects_by_type(&objects));

        let mut game_info_value = Value::Object(game_info);

        if self.game_meta.start_time.is_none() && game_engine_can_score {
            self.game_meta.start_time = Some(start_time_string);
        }

        let game_id = if game_engine_can_score {
            self.game_meta
                .start_time
                .as_ref()
                .map(|start| start.replace(' ', "_").replace(':', "-").split('.').next().unwrap_or("").to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        game_info_value
            .as_object_mut()
            .unwrap()
            .insert("game_id".to_string(), Value::String(game_id));
        let map_resolution_inputs = self.get_map_resolution_inputs(&game_info_value);
        let spawn_hash = if self.config.compute_spawn_parameters_hash {
            Value::String(calculate_spawn_hash(&map_resolution_inputs)?)
        } else {
            Value::Null
        };
        let map = game_info_value.as_object_mut().unwrap();
        map.insert("map_resolution_inputs".to_string(), map_resolution_inputs);
        map.insert("spawn_parameters_hash".to_string(), spawn_hash);

        Ok(game_info_value)
    }

    pub fn extract_events(&mut self, old_game_info: &Value, new_game_info: &mut Value) -> ExtractResult {
        let result = self.game_meta.extract_events(old_game_info, new_game_info);
        if result.clear_caches {
            self.clear_caches();
        }
        result
    }

    fn read_utf16_string(&mut self, address: u64, len: usize) -> Result<String> {
        let bytes = self.mem.read_bytes(address, len)?;
        Ok(read_utf16_from_bytes(&bytes))
    }

    fn object_string_from_type(&mut self, object_type: u8) -> Result<String> {
        if let Some(value) = self.object_type_string_cache.get(&object_type) {
            return Ok(value.clone());
        }
        let object_type_definitions_array = 0x1FCB78u64;
        let type_def_addr = self
            .mem
            .read_u32(object_type_definitions_array + 4 * object_type as u64)?;
        let type_string_ptr = self.mem.read_u32(type_def_addr as u64)? as u64;
        let type_string = self.mem.read_string(type_string_ptr, 128)?;
        self.object_type_string_cache
            .insert(object_type, type_string.clone());
        Ok(type_string)
    }

    fn get_model_nodes(&mut self, base_address: u64) -> Result<Vec<Value>> {
        let model_node_start_offset = 0x4a8 - 0x28;
        let model_node_stride = 0x34usize;
        let model_node_count = 19usize;
        let data = self
            .mem
            .read_bytes(base_address + model_node_start_offset, model_node_stride * model_node_count)?;
        let mut model_nodes = Vec::with_capacity(model_node_count);
        for i in 0..model_node_count {
            let base = model_node_stride * i;
            let mut values = Vec::with_capacity(13);
            for j in 0..13 {
                let offset = base + j * 4;
                values.push(f32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()));
            }
            let mut node = Vec::with_capacity(13);
            for value in &values[10..13] {
                node.push(f32_value(*value));
            }
            node.push(f32_value(values[0]));
            for value in &values[1..10] {
                node.push(f32_value(*value));
            }
            model_nodes.push(Value::Array(node));
        }
        Ok(model_nodes)
    }

    fn get_weapon(&mut self, weapon_object_handle: u32, object_header_datum_array: u32) -> Result<Value> {
        if weapon_object_handle == 0xFFFF_FFFF {
            return Ok(Value::Object(Map::new()));
        }
        let first_object = self.mem.read_u32(object_header_datum_array as u64 + 52)?;
        let weapon_object_address = self.mem.read_u32(
            first_object as u64 + 12 * (weapon_object_handle & 0xFFFF) as u64 + 8,
        )?;
        if weapon_object_address == 0 {
            return Ok(Value::Object(Map::new()));
        }
        let tag_address = (32i64 * self.mem.read_i16(weapon_object_address as u64)? as i64
            + self.globals.global_tag_instances_address as i64) as u64;
        let tag_definition = self.mem.read_u32(tag_address + 20)? as u64;
        let weapon_type = self.mem.read_u8(tag_definition + 0x309)?;
        let is_energy_weapon = (weapon_type & 8) != 0;
        let mut map = Map::new();
        map.insert(
            "heat_meter".to_string(),
            f32_value(self.mem.read_f32(weapon_object_address as u64 + 0xD4)?),
        );
        map.insert(
            "used_energy".to_string(),
            f32_value(self.mem.read_f32(weapon_object_address as u64 + 0xE0)?),
        );
        map.insert(
            "charge_amount".to_string(),
            f32_value(self.mem.read_f32(weapon_object_address as u64 + 0xF0)?),
        );
        map.insert(
            "reloading".to_string(),
            json!(self.mem.read_u8(weapon_object_address as u64 + 0x258)?),
        );
        map.insert(
            "can_fire".to_string(),
            json!(self.mem.read_u8(weapon_object_address as u64 + 0x259)?),
        );
        map.insert(
            "reload_time".to_string(),
            json!(self.mem.read_i16(weapon_object_address as u64 + 0x25A)?),
        );
        map.insert(
            "backpack_ammo_count".to_string(),
            json!(self.mem.read_i16(weapon_object_address as u64 + 0x25E)?),
        );
        map.insert(
            "magazine_ammo_count".to_string(),
            json!(self.mem.read_i16(weapon_object_address as u64 + 0x260)?),
        );
        let tag_host = self.mem.known_host_address(tag_address)
            .or_else(|_| self.mem.get_host_address(tag_address))?;
        map.insert(
            "weapon_tag_address".to_string(),
            Value::String(format!(
                "{} @ {} -> {}",
                self.mem.read_u32(tag_address)?,
                py_hex_u64(tag_address),
                py_hex_u64(tag_host)
            )),
        );
        map.insert(
            "energy_used".to_string(),
            f32_value(self.mem.read_f32(weapon_object_address as u64 + 0x1F0)?),
        );
        map.insert("weapon_type".to_string(), json!(weapon_type));
        map.insert("is_energy_weapon".to_string(), json!(is_energy_weapon));
        map.insert("zoom_levels".to_string(), json!(self.mem.read_i16(tag_definition + 986)?));
        map.insert("zoom_min".to_string(), f32_value(self.mem.read_f32(tag_definition + 988)?));
        map.insert("zoom_max".to_string(), f32_value(self.mem.read_f32(tag_definition + 992)?));
        map.insert(
            "autoaim_angle".to_string(),
            f32_value(self.mem.read_f32(tag_definition + 996)?),
        );
        map.insert(
            "autoaim_range".to_string(),
            f32_value(self.mem.read_f32(tag_definition + 1000)?),
        );
        map.insert(
            "magnetism_angle".to_string(),
            f32_value(self.mem.read_f32(tag_definition + 1004)?),
        );
        map.insert(
            "magnetism_range".to_string(),
            f32_value(self.mem.read_f32(tag_definition + 1008)?),
        );
        map.insert(
            "deviation_angle".to_string(),
            f32_value(self.mem.read_f32(tag_definition + 1012)?),
        );
        let tag_name_ptr = self.mem.read_u32(tag_address + 0x10)?;
        map.insert(
            "tag_name".to_string(),
            Value::String(self.mem.read_string(tag_name_ptr as u64, 128)?),
        );
        map.insert("object_id".to_string(), json!(weapon_object_handle & 0xFFFF));
        Ok(Value::Object(map))
    }

    fn get_weapons(&mut self, first_weapon_address: u64, object_header_datum_array: u32) -> Result<Vec<Value>> {
        let mut weapons = Vec::new();
        for weapon_index in 0..4u64 {
            let weapon_handle = self.mem.read_u32(first_weapon_address + 4 * weapon_index)?;
            let weapon = self.get_weapon(weapon_handle, object_header_datum_array)?;
            if weapon.as_object().map(|object| !object.is_empty()).unwrap_or(false) {
                weapons.push(weapon);
            }
        }
        Ok(weapons)
    }

    fn get_animation_debug_info(
        &mut self,
        unk_handle: u32,
        animation_id: i16,
        animation_tick: i16,
    ) -> Result<Value> {
        let tag_address = self.mem.read_u32(
            32 * (unk_handle & 0xFFFF) as u64 + self.globals.global_tag_instances_address as u64 + 20,
        )?;
        let animation_address = (self.mem.read_u32(tag_address as u64 + 120)? as i64
            + 180 * animation_id as i64) as u64;
        let animation_length = self.mem.read_i16(animation_address + 34)?;
        let unk_46 = self.mem.read_i16(animation_address + 46)?;
        let unk_52 = self.mem.read_i16(animation_address + 52)?;
        let unk_54 = self.mem.read_i16(animation_address + 54)?;
        let result = if animation_tick < animation_length {
            if animation_tick != animation_length || unk_46 != 0 {
                i32::from(animation_tick + 1 == unk_52 || animation_tick == unk_54)
            } else {
                2
            }
        } else if unk_46 <= 0 {
            3
        } else {
            4
        };
        Ok(json!({
            "tag_address": py_hex_u32(tag_address),
            "animation_address": py_hex_u64(animation_address),
            "animation_length": animation_length,
            "unk_handle": py_hex_u32(unk_handle),
            "animation_id": animation_id,
            "animation_tick": animation_tick,
            "unk_46": unk_46,
            "unk_52": unk_52,
            "unk_54": unk_54,
            "result": result,
        }))
    }

    fn get_player_object_data(
        &mut self,
        dynamic_player_address: u64,
        object_header_datum_array: u32,
        player_object_debug: &mut Map<String, Value>,
    ) -> Result<Value> {
        let dynamic_tag_ref = self.mem.read_u32(dynamic_player_address)?;
        let biped_tag_address = self.mem.read_u32(
            32 * (dynamic_tag_ref & 0xFFFF) as u64
                + self.globals.global_tag_instances_address as u64
                + 0x14,
        )? as u64;
        let biped_camera_height_standing = self.mem.read_f32(biped_tag_address + 0x400)?;
        let biped_camera_height_crouching = self.mem.read_f32(biped_tag_address + 0x404)?;
        let crouchscale = self.mem.read_f32(dynamic_player_address + 0x464)?;
        player_object_debug.insert(
            "biped_tag_address".to_string(),
            Value::String(format!(
                "{} -> {}",
                py_hex_u64(biped_tag_address),
                py_hex_u64(self.mem.get_host_address(biped_tag_address)?)
            )),
        );

        let melee_remaining = self.mem.read_u8(dynamic_player_address + 0x45D)?;
        let melee_damage_tick = self.mem.read_u8(dynamic_player_address + 0x45E)?;
        let animation_handle = self.mem.read_u32(dynamic_player_address + 0x7C)?;
        let animation_id = self.mem.read_i16(dynamic_player_address + 0x80)?;
        let animation_tick = self.mem.read_i16(dynamic_player_address + 0x82)?;
        let animation_debug =
            self.get_animation_debug_info(animation_handle, animation_id, animation_tick)?;
        let weapons = self.get_weapons(dynamic_player_address + 0x2A8, object_header_datum_array)?;
        let mut map = Map::new();
        macro_rules! insert_u32 {
            ($name:literal, $offset:expr) => {
                map.insert($name.to_string(), json!(self.mem.read_u32(dynamic_player_address + $offset)?));
            };
        }
        macro_rules! insert_u16 {
            ($name:literal, $offset:expr) => {
                map.insert($name.to_string(), json!(self.mem.read_u16(dynamic_player_address + $offset)?));
            };
        }
        macro_rules! insert_i16 {
            ($name:literal, $offset:expr) => {
                map.insert($name.to_string(), json!(self.mem.read_i16(dynamic_player_address + $offset)?));
            };
        }
        macro_rules! insert_i32 {
            ($name:literal, $offset:expr) => {
                map.insert($name.to_string(), json!(self.mem.read_i32(dynamic_player_address + $offset)?));
            };
        }
        macro_rules! insert_u8 {
            ($name:literal, $offset:expr) => {
                map.insert($name.to_string(), json!(self.mem.read_u8(dynamic_player_address + $offset)?));
            };
        }
        macro_rules! insert_f32 {
            ($name:literal, $offset:expr) => {
                map.insert($name.to_string(), f32_value(self.mem.read_f32(dynamic_player_address + $offset)?));
            };
        }

        insert_u32!("flags", 0x4);
        insert_f32!("x", 0xC);
        insert_f32!("y", 0x10);
        insert_f32!("z", 0x14);
        insert_f32!("x_vel", 0x18);
        insert_f32!("y_vel", 0x1C);
        insert_f32!("z_vel", 0x20);
        insert_f32!("legs_pitch", 0x24);
        insert_f32!("legs_yaw", 0x28);
        insert_f32!("legs_roll", 0x2C);
        insert_f32!("pitch1", 0x30);
        insert_f32!("yaw1", 0x34);
        insert_f32!("roll1", 0x38);
        insert_f32!("ang_vel_x", 0x3C);
        insert_f32!("ang_vel_y", 0x40);
        insert_f32!("ang_vel_z", 0x44);
        insert_f32!("aim_assist_sphere_x", 0x50);
        insert_f32!("aim_assist_sphere_y", 0x54);
        insert_f32!("aim_assist_sphere_z", 0x58);
        insert_f32!("aim_assist_sphere_radius", 0x5C);
        insert_f32!("scale", 0x60);
        insert_u16!("type", 0x64);
        insert_u16!("render_flags", 0x66);
        insert_i16!("weapon_owner_team", 0x68);
        insert_i16!("powerup_unk2", 0x6A);
        insert_i16!("idle_ticks", 0x6C);
        insert_f32!("max_health", 0x88);
        insert_f32!("max_shields", 0x8C);
        insert_f32!("health", 0x90);
        insert_f32!("shields", 0x94);
        insert_f32!("unk_dmg_countdown_0x98", 0x98);
        insert_f32!("unk_dmg_countdown_0x9C", 0x9C);
        insert_f32!("unk_dmg_countdown_0xA4", 0xA4);
        insert_f32!("unk_dmg_countdown_0xA8", 0xA8);
        insert_i32!("unk3", 0xAC);
        insert_i32!("unk4", 0xB0);
        insert_u16!("shields_charge_delay", 0xB4);
        let shields_status = self.mem.read_u16(dynamic_player_address + 0xB6)?;
        map.insert("shields_status".to_string(), json!(shields_status));
        map.insert("shields_status_hex".to_string(), Value::String(py_hex_u32(shields_status as u32)));
        insert_i32!("next_object", 0xC4);
        map.insert(
            "next_object_2".to_string(),
            Value::String(py_hex_u32(self.mem.read_u32(dynamic_player_address + 0xC8)?)),
        );
        map.insert(
            "parent_object".to_string(),
            Value::String(py_hex_i32(self.mem.read_i32(dynamic_player_address + 0xCC)?)),
        );
        insert_u8!("camo", 0x1B4);
        insert_u8!("flashlight", 0x1B6);
        insert_u32!("current_action", 0x1B8);
        insert_f32!("stunned", 0x3D4);
        insert_f32!("xunk0", 0x1D4);
        insert_f32!("yunk0", 0x1D8);
        insert_f32!("zunk0", 0x1DC);
        insert_f32!("xaima", 0x1E0);
        insert_f32!("yaima", 0x1E4);
        insert_f32!("zaima", 0x1E8);
        insert_f32!("aiming_vector_x", 0x1EC);
        insert_f32!("aiming_vector_y", 0x1F0);
        insert_f32!("aiming_vector_z", 0x1F4);
        insert_f32!("xaim0", 0x1F8);
        insert_f32!("yaim0", 0x1FC);
        insert_f32!("zaim0", 0x200);
        insert_f32!("xaim1", 0x204);
        insert_f32!("yaim1", 0x208);
        insert_f32!("zaim1", 0x20C);
        insert_f32!("looking_vector_x", 0x210);
        insert_f32!("looking_vector_y", 0x214);
        insert_f32!("looking_vector_z", 0x218);
        insert_f32!("move_forward", 0x228);
        insert_f32!("move_left", 0x22C);
        insert_f32!("move_up", 0x230);
        insert_u8!("melee_damage_type", 0x239);
        insert_u8!("animation_1", 0x253);
        insert_u8!("animation_2", 0x254);
        map.insert("animation_debug".to_string(), animation_debug);
        insert_i16!("selected_weapon_index", 0x2A2);
        map.insert("weapons".to_string(), Value::Array(weapons));
        map.insert(
            "current_equipment".to_string(),
            Value::String(py_hex_u32(self.mem.read_u32(dynamic_player_address + 0x2C8)?)),
        );
        insert_u8!("primary_nades", 0x2CE);
        insert_u8!("secondary_nades", 0x2CF);
        map.insert("zoom_level".to_string(), json!(self.mem.read_i8(dynamic_player_address + 0x2D0)?));
        insert_f32!("camo_amount", 0x32C);
        insert_u16!("camo_self_revealed", 0x3D2);
        map.insert(
            "damagers_list_address".to_string(),
            Value::String(py_hex_u64(self.mem.get_host_address(dynamic_player_address + 0x3E0)?)),
        );
        map.insert("crouchscale".to_string(), f32_value(crouchscale));
        insert_f32!("facing1", 0x46C);
        insert_f32!("facing2", 0x470);
        insert_f32!("facing3", 0x474);
        map.insert("camera_x".to_string(), f32_value(self.mem.read_f32(dynamic_player_address + 0xC)?));
        map.insert("camera_y".to_string(), f32_value(self.mem.read_f32(dynamic_player_address + 0x10)?));
        let camera_z = (1.0 - crouchscale) * biped_camera_height_standing
            + crouchscale * biped_camera_height_crouching
            + self.mem.read_f32(dynamic_player_address + 0x14)?;
        map.insert("camera_z".to_string(), f32_value(camera_z));
        insert_i16!("air_1_0x64", 0x64);
        insert_u8!("airborne", 0x424);
        insert_u8!("landing_stun_current_duration", 0x428);
        insert_u8!("landing_stun_target_duration", 0x429);
        insert_u8!("airborne_ticks", 0x459);
        insert_u8!("slipping_ticks", 0x45A);
        insert_u8!("stop_ticks", 0x45B);
        insert_u8!("jump_recovery_timer", 0x45C);
        map.insert("melee_animation_remaining".to_string(), json!(melee_remaining));
        map.insert("melee_animation_damage_tick".to_string(), json!(melee_damage_tick));
        map.insert(
            "melee_impact_this_tick".to_string(),
            json!(melee_remaining == melee_damage_tick),
        );
        insert_u16!("landing", 0x45F);
        insert_i16!("air_3_0x460", 0x460);
        insert_i16!("air_4_0xB6", 0xB6);
        map.insert("biped_flags".to_string(), json!(self.mem.read_u32(biped_tag_address + 0x2F4)?));
        map.insert(
            "autoaim_pill_radius".to_string(),
            f32_value(self.mem.read_f32(biped_tag_address + 0x458)?),
        );
        Ok(Value::Object(map))
    }

    fn get_player_ui_globals(&mut self, local_player: i16) -> Result<Value> {
        if local_player == -1 {
            return Ok(Value::Object(Map::new()));
        }
        let player_ui_globals_address = 0x2E40D0u64;
        let base = player_ui_globals_address + local_player as u64 * 56;
        Ok(json!({
            "address": py_hex_u64(self.mem.get_host_address(base)?),
            "color": self.mem.read_u8(base + 24)?,
            "button_config": self.mem.read_u8(base + 40)?,
            "joystick_config": self.mem.read_u8(base + 41)?,
            "sensitivity": self.mem.read_u8(base + 42)?,
            "joystick_inverted": self.mem.read_u8(base + 43)?,
            "rumble_enabled": self.mem.read_u8(base + 44)?,
            "flight_inverted": self.mem.read_u8(base + 45)?,
            "autocenter_enabled": self.mem.read_u8(base + 46)?,
            "active_player_profile_index": py_hex_u32(self.mem.read_u32(base + 48)?),
            "joined_multiplayer_game": self.mem.read_u8(base + 52)?,
        }))
    }

    fn signed_index_addr(base: u64, stride: i64, index: i16, offset: i64) -> u64 {
        (base as i64 + stride * index as i64 + offset) as u64
    }

    fn get_input_data(&mut self, local_player_index: i16, player_id: u32) -> Result<Value> {
        let player_control_address = self.mem.read_u32(0x276794)?;
        let control_globals = (|| -> Result<(u32, f32, f32)> {
            let global_game_globals_address = self.mem.read_u32(0x39BE4C)?;
            let game_globals_player_control_address =
                self.mem.read_u32(global_game_globals_address as u64 + 276)?;
            let global_friction = self.mem.read_f32(game_globals_player_control_address as u64)?;
            let global_adhesion = self.mem.read_f32(game_globals_player_control_address as u64 + 0x4)?;
            Ok((
                game_globals_player_control_address,
                global_friction,
                global_adhesion,
            ))
        })();
        let (_game_globals_player_control_address, global_friction, global_adhesion) = match control_globals {
            Ok(values) => values,
            Err(err) => {
                eprintln!("Failed to read player control globals: {err:#}");
                return Ok(Value::Object(Map::new()));
            }
        };

        let update_queue_address = self.mem.read_u32(0x2E8870)?;
        let update_client_player_address =
            self.mem.read_u32(update_queue_address as u64 + 0x34)?;
        let player_queue_base = update_client_player_address as u64 + 0x28 * player_id as u64;
        let button_field = self.mem.read_u8(player_queue_base + 0x4)?;
        let action_field = self.mem.read_u8(player_queue_base + 0x5)?;
        let local_i = local_player_index;
        let local_28 = |base: u64, offset: i64| Self::signed_index_addr(base, 0x1C, local_i, offset);
        let gamepad = |base: u64, offset: i64| Self::signed_index_addr(base, 0x28, local_i, offset);
        let control = |offset: i64| (player_control_address as i64 + ((local_i as i64) << 6) + offset) as u64;

        let mut map = Map::new();
        map.insert("local_player_index".to_string(), json!(local_player_index));
        map.insert(
            "look_yaw_rate".to_string(),
            f32_value(self.mem.read_f32((0x2E4684i64 + 4 * local_i as i64) as u64)?),
        );
        map.insert(
            "look_pitch_rate".to_string(),
            f32_value(self.mem.read_f32((0x2E4694i64 + 4 * local_i as i64) as u64)?),
        );
        map.insert(
            "input_abstraction_globals".to_string(),
            Value::String(format!(
                "{} @ {}",
                py_hex_u32(self.mem.read_u32(0x2E45A0)?),
                py_hex_u64(self.mem.get_host_address(0x2E45A0)?)
            )),
        );
        map.insert(
            "player_control_pointer".to_string(),
            Value::String(format!(
                "{} @ {}",
                py_hex_u32(player_control_address),
                py_hex_u64(self.mem.get_host_address(0x276794)?)
            )),
        );
        map.insert(
            "player_control".to_string(),
            Value::String(format!(
                "{} @ {}",
                py_hex_u32(self.mem.read_u32(player_control_address as u64)?),
                py_hex_u64(self.mem.get_host_address(player_control_address as u64)?)
            )),
        );

        if local_player_index != -1 {
            let aim_assist_far = self.mem.read_f32(control(16 + 0x30))?;
            map.insert(
                "player_control_state".to_string(),
                json!({
                    "player_desired_yaw": self.mem.read_f32(control(0x1C))?,
                    "player_desired_pitch": self.mem.read_f32(control(0x20))?,
                    "player_zoom_level": self.mem.read_i16(control(16 + 0x24))?,
                    "player_aim_assist_target": py_hex_u32(self.mem.read_u32(control(16 + 0x28))?),
                    "player_aim_assist_near": self.mem.read_f32(control(16 + 0x2C))?,
                    "player_aim_assist_far": aim_assist_far,
                    "global_friction": global_friction,
                    "global_adhesion": global_adhesion,
                    "friction_coeff": 1.0 - aim_assist_far * global_friction,
                    "adhesion_coeff": aim_assist_far * global_adhesion,
                }),
            );
        } else {
            map.insert("player_control_state".to_string(), Value::Object(Map::new()));
        }

        if local_player_index != -1 {
            map.insert(
                "input_abstraction_input_state".to_string(),
                json!({
                    "address": py_hex_u64(self.mem.get_host_address(0x2E4600)?),
                    "a": self.mem.read_u8(local_28(0x2E4600, 0x0))?,
                    "black": self.mem.read_u8(local_28(0x2E4600, 0x1))?,
                    "x": self.mem.read_u8(local_28(0x2E4600, 0x2))?,
                    "y": self.mem.read_u8(local_28(0x2E4600, 0x3))?,
                    "b": self.mem.read_u8(local_28(0x2E4600, 0x4))?,
                    "white": self.mem.read_u8(local_28(0x2E4600, 0x5))?,
                    "left_trigger": self.mem.read_u8(local_28(0x2E4600, 0x6))?,
                    "right_trigger": self.mem.read_u8(local_28(0x2E4600, 0x7))?,
                    "start": self.mem.read_u8(local_28(0x2E4600, 0x8))?,
                    "back": self.mem.read_u8(local_28(0x2E4600, 0x9))?,
                    "left_stick_button": self.mem.read_u8(local_28(0x2E4600, 0xA))?,
                    "right_stick_button": self.mem.read_u8(local_28(0x2E4600, 0xB))?,
                    "left_stick_vertical": self.mem.read_f32(local_28(0x2E4600, 0xC))?,
                    "left_stick_horizontal": self.mem.read_f32(local_28(0x2E4600, 0x10))?,
                    "right_stick_horizontal": self.mem.read_f32(local_28(0x2E4600, 0x14))?,
                    "right_stick_vertical": self.mem.read_f32(local_28(0x2E4600, 0x18))?,
                }),
            );
            map.insert(
                "input_gamepad_state".to_string(),
                json!({
                    "address": py_hex_u64(self.mem.get_host_address(gamepad(0x276AFC, 0))?),
                    "address2": py_hex_u64(self.mem.get_host_address(gamepad(0x276A5C, 0))?),
                    "a": self.mem.read_u8(gamepad(0x276A5C, 0x0))?,
                    "b": self.mem.read_u8(gamepad(0x276A5C, 0x1))?,
                    "x": self.mem.read_u8(gamepad(0x276A5C, 0x2))?,
                    "y": self.mem.read_u8(gamepad(0x276A5C, 0x3))?,
                    "black": self.mem.read_u8(gamepad(0x276A5C, 0x4))?,
                    "white": self.mem.read_u8(gamepad(0x276A5C, 0x5))?,
                    "left_trigger": self.mem.read_u8(gamepad(0x276A5C, 0x6))?,
                    "right_trigger": self.mem.read_u8(gamepad(0x276A5C, 0x7))?,
                    "a_duration": self.mem.read_u8(gamepad(0x276A5C, 0x10))?,
                    "b_duration": self.mem.read_u8(gamepad(0x276A5C, 0x11))?,
                    "x_duration": self.mem.read_u8(gamepad(0x276A5C, 0x12))?,
                    "y_duration": self.mem.read_u8(gamepad(0x276A5C, 0x13))?,
                    "black_duration": self.mem.read_u8(gamepad(0x276A5C, 0x14))?,
                    "white_duration": self.mem.read_u8(gamepad(0x276A5C, 0x15))?,
                    "left_trigger_duration": self.mem.read_u8(gamepad(0x276A5C, 0x16))?,
                    "right_trigger_duration": self.mem.read_u8(gamepad(0x276A5C, 0x17))?,
                    "dpad_up_duration": self.mem.read_u8(gamepad(0x276A5C, 0x18))?,
                    "dpad_down_duration": self.mem.read_u8(gamepad(0x276A5C, 0x19))?,
                    "dpad_left_duration": self.mem.read_u8(gamepad(0x276A5C, 0x1A))?,
                    "dpad_right_duration": self.mem.read_u8(gamepad(0x276A5C, 0x1B))?,
                    "left_stick_duration": self.mem.read_u8(gamepad(0x276A5C, 0x1E))?,
                    "right_stick_duration": self.mem.read_u8(gamepad(0x276A5C, 0x1F))?,
                    "left_stick_horizontal": self.mem.read_i16(gamepad(0x276A5C, 0x20))?,
                    "left_stick_vertical": self.mem.read_i16(gamepad(0x276A5C, 0x22))?,
                    "right_stick_horizontal": self.mem.read_i16(gamepad(0x276A5C, 0x24))?,
                    "right_stick_vertical": self.mem.read_i16(gamepad(0x276A5C, 0x26))?,
                }),
            );
        } else {
            map.insert("input_abstraction_input_state".to_string(), Value::Object(Map::new()));
            map.insert("input_gamepad_state".to_string(), Value::Object(Map::new()));
        }

        map.insert(
            "update_queue_values".to_string(),
            json!({
                "address": py_hex_u64(self.mem.get_host_address(player_queue_base)?),
                "unit_ref": py_hex_u32(self.mem.read_u16(player_queue_base)? as u32),
                "button_field": py_hex_u32(button_field as u32),
                "button_crouch": button_field & 0x1,
                "button_jump": button_field & 0x2,
                "button_fire": button_field & 0x8,
                "button_flashlight": button_field & 0x10,
                "button_reload": button_field & 0x40,
                "button_melee": button_field & 0x80,
                "action_field": py_hex_u32(action_field as u32),
                "button_throw_grenade": action_field & 0x30,
                "button_action": action_field & 0x40,
                "desired_yaw": self.mem.read_f32(player_queue_base + 0xC)?,
                "desired_pitch": self.mem.read_f32(player_queue_base + 0x10)?,
                "forward": self.mem.read_f32(player_queue_base + 0x14)?,
                "left": self.mem.read_f32(player_queue_base + 0x18)?,
                "right_trigger_held": self.mem.read_f32(player_queue_base + 0x1C)?,
                "desired_weapon": self.mem.read_u16(player_queue_base + 0x20)?,
                "desired_grenades": self.mem.read_u16(player_queue_base + 0x22)?,
                "zoom_level": self.mem.read_i16(player_queue_base + 0x24)?,
            }),
        );
        map.insert(
            "player_ui_globals".to_string(),
            self.get_player_ui_globals(local_player_index)?,
        );
        Ok(Value::Object(map))
    }

    fn get_first_person_weapon(&mut self, local_player_index: i16) -> Result<Value> {
        let weapon_address = self.mem.read_u32(0x276B48)? as u64 + 7840 * local_player_index as u64;
        Ok(json!({
            "address": format!("{:#x} -> {:#x}", weapon_address, self.mem.get_host_address(weapon_address)?),
            "weapon_rendered": self.mem.read_u32(weapon_address)?,
            "player_object": format!("{:#x}", self.mem.read_u32(weapon_address + 4)?),
            "weapon_object": format!("{:#x}", self.mem.read_u32(weapon_address + 8)?),
            "weapon_object_id": self.mem.read_u32(weapon_address + 8)? & 0xFFFF,
            "state": self.mem.read_i16(weapon_address + 12)?,
            "idle_animation_threshold": self.mem.read_i16(weapon_address + 14)?,
            "idle_animation_counter": self.mem.read_i16(weapon_address + 16)?,
            "animation_id": self.mem.read_i16(weapon_address + 22)?,
            "animation_tick": self.mem.read_i16(weapon_address + 24)?,
        }))
    }

    fn get_observer_camera_info(&mut self, local_player_index: i16) -> Result<Value> {
        if local_player_index == -1 {
            return Ok(Value::Object(Map::new()));
        }
        let observer_camera_address = 0x271550u64 + 167 * 4 * local_player_index as u64;
        Ok(json!({
            "address": format!("{:#x} -> {:#x}", observer_camera_address, self.mem.get_host_address(observer_camera_address)?),
            "x": self.mem.read_f32(observer_camera_address)?,
            "y": self.mem.read_f32(observer_camera_address + 4)?,
            "z": self.mem.read_f32(observer_camera_address + 8)?,
            "x_vel": self.mem.read_f32(observer_camera_address + 20)?,
            "y_vel": self.mem.read_f32(observer_camera_address + 24)?,
            "z_vel": self.mem.read_f32(observer_camera_address + 28)?,
            "x_aim": self.mem.read_f32(observer_camera_address + 32)?,
            "y_aim": self.mem.read_f32(observer_camera_address + 36)?,
            "z_aim": self.mem.read_f32(observer_camera_address + 40)?,
            "fov": self.mem.read_f32(observer_camera_address + 56)?,
        }))
    }

    fn get_game_time_info(&mut self) -> Result<Value> {
        let addr = self.globals.game_time_globals_address as u64;
        let raw_game_time = self.mem.read_u32(addr + 12)?;
        let host = self.mem.known_host_address(addr).or_else(|_| self.mem.get_host_address(addr))?;
        Ok(json!({
            "game_time_globals_address": self.globals.game_time_globals_address,
            "game_time_initialized": self.mem.read_u8(addr)?,
            "game_time_active": self.mem.read_u8(addr + 1)?,
            "game_time_paused": self.mem.read_u8(addr + 2)?,
            "game_time_monitor_state": self.mem.read_i16(addr + 4)?,
            "game_time_monitor_counter": self.mem.read_i16(addr + 6)?,
            "game_time_monitor_latency": self.mem.read_i16(addr + 8)?,
            "game_time": raw_game_time as i64 - 1,
            "game_time_elapsed": self.mem.read_u32(addr + 16)?,
            "game_time_speed": self.mem.read_f32(addr + 24)?,
            "game_time_leftover_dt": self.mem.read_f32(addr + 28)?,
            "update_client_maximum_actions": self.mem.read_u32(0x2E87E8)? as i64 - self.mem.read_u32(0x2E87E4)? as i64 + 1,
            "game_time_globals_address_hex": format!("{:#x} -> {:#x}", self.globals.game_time_globals_address, host),
            "real_time_elapsed": timedelta_seconds_floor(raw_game_time as u64 / 30),
        }))
    }

    fn get_map_info(&mut self) -> Result<Value> {
        let map_header_address = 0x2DFC98u64 - 4820;
        Ok(json!({
            "address": py_hex_u64(self.mem.get_host_address(map_header_address)?),
            "cache_version": self.mem.read_u32(map_header_address + 0x4)?,
            "file_size": self.mem.read_u32(map_header_address + 0x8)?,
            "padding_length": self.mem.read_u32(map_header_address + 0xC)?,
            "tag_data_offset": self.mem.read_u32(map_header_address + 0x10)?,
            "tag_data_size": self.mem.read_u32(map_header_address + 0x14)?,
            "scenario_name": self.mem.read_string(map_header_address + 0x20, 32)?,
            "build_version": self.mem.read_string(map_header_address + 0x40, 32)?,
            "scenario_type": self.mem.read_u16(map_header_address + 0x60)?,
            "checksum": self.mem.read_u32(map_header_address + 0x64)?,
        }))
    }

    fn get_memory_info(&mut self) -> Result<Value> {
        let game_state_base = self.mem.read_u32(0x2E2D14)?;
        let tag_cache_base = self.mem.read_u32(0x2E2D18)?;
        let texture_cache_base = self.mem.read_u32(0x2E2D1C)?;
        let sound_cache_base = self.mem.read_u32(0x2E2D20)?;
        Ok(json!({
            "game_state_base_address": format!("{} -> {}", py_hex_u32(game_state_base), py_hex_u64(self.mem.get_host_address(game_state_base as u64)?)),
            "tag_cache_base_address": format!("{} -> {}", py_hex_u32(tag_cache_base), py_hex_u64(self.mem.get_host_address(tag_cache_base as u64)?)),
            "texture_cache_base_address": format!("{} -> {}", py_hex_u32(texture_cache_base), py_hex_u64(self.mem.get_host_address(texture_cache_base as u64)?)),
            "sound_cache_base_address": format!("{} -> {}", py_hex_u32(sound_cache_base), py_hex_u64(self.mem.get_host_address(sound_cache_base as u64)?)),
            "game_state_size": format!("{:#x}", self.mem.read_u32(0x32E4A)?),
            "tag_cache_size": format!("{:#x}", self.mem.read_u32(0x32E5D)?),
            "texture_cache_size": format!("{:#x}", self.mem.read_u32(0x32E75)?),
            "sound_cache_size": format!("{:#x}", self.mem.read_u32(0x32E8A)?),
        }))
    }

    fn get_flag_data(&mut self) -> Result<Value> {
        let game_engine_globals_address = self.mem.read_u32(0x2F9110)?;
        if game_engine_globals_address != 0 && self.mem.read_u32(game_engine_globals_address as u64 + 0x4)? == 1 {
            let flag_0 = self.mem.read_u32(0x2762A4)? as u64;
            let flag_1 = self.mem.read_u32(0x2762A4 + 4)? as u64;
            Ok(json!({
                "flag_base_0": {
                    "x": self.mem.read_f32(flag_0)?,
                    "y": self.mem.read_f32(flag_0 + 4)?,
                    "z": self.mem.read_f32(flag_0 + 8)?,
                },
                "flag_base_1": {
                    "x": self.mem.read_f32(flag_1)?,
                    "y": self.mem.read_f32(flag_1 + 4)?,
                    "z": self.mem.read_f32(flag_1 + 8)?,
                },
            }))
        } else {
            Ok(Value::Object(Map::new()))
        }
    }

    fn get_network_game_data(&mut self, network_game_data_address: u64) -> Result<Value> {
        let machine_count = self.mem.read_i16(network_game_data_address + 274)?;
        let network_machines_address = network_game_data_address + 276;
        let player_count = self.mem.read_i16(network_game_data_address + 548)?;
        let network_players_address = network_game_data_address + 550;

        let mut network_machines = Vec::new();
        for i in 0..machine_count.max(0) as u64 {
            network_machines.push(json!({
                "name": self.mem.read_wchar(network_machines_address + 68 * i, 68)?,
                "machine_index": self.mem.read_u8(network_machines_address + 68 * i + 64)?,
            }));
        }

        let mut network_players = Vec::new();
        for i in 0..player_count.max(0) as u64 {
            network_players.push(json!({
                "name": self.mem.read_wchar(network_players_address + 32 * i, 24)?,
                "color": self.mem.read_i16(network_players_address + 32 * i + 24)?,
                "unused": self.mem.read_i16(network_players_address + 32 * i + 26)?,
                "machine_index": self.mem.read_u8(network_players_address + 32 * i + 28)?,
                "controller_index": self.mem.read_u8(network_players_address + 32 * i + 29)?,
                "team": self.mem.read_u8(network_players_address + 32 * i + 30)?,
                "player_list_index": self.mem.read_u8(network_players_address + 32 * i + 31)?,
            }));
        }

        Ok(json!({
            "player_count": player_count,
            "maximum_player_count": self.mem.read_u8(network_game_data_address + 270)?,
            "machine_count": machine_count,
            "network_machines": network_machines,
            "network_players": network_players,
        }))
    }

    fn get_network_game_client(&mut self) -> Result<Value> {
        let network_game_client_address = 0x2FB180u64;
        Ok(json!({
            "machine_index": self.mem.read_u16(network_game_client_address)?,
            "advertised_games": {},
            "ping_target_ip": py_hex_i32(self.mem.read_i32(network_game_client_address + 2056)?),
            "packets_sent": self.mem.read_i16(network_game_client_address + 2084)?,
            "packets_received": self.mem.read_i16(network_game_client_address + 2086)?,
            "average_ping": self.mem.read_i16(network_game_client_address + 2088)?,
            "ping_active": self.mem.read_u8(network_game_client_address + 2090)?,
            "seconds_to_game_start": self.mem.read_i16(network_game_client_address + 3236)?,
            "network_game_data": self.get_network_game_data(network_game_client_address + 2140)?,
        }))
    }

    fn get_network_game_server(&mut self) -> Result<Value> {
        let network_game_server_address = 0x2FBE40u64;
        Ok(json!({
            "address": py_hex_u64(self.mem.get_host_address(network_game_server_address)?),
            "countdown_active": self.mem.read_u8(network_game_server_address + 1172)?,
            "countdown_paused": self.mem.read_u8(network_game_server_address + 1173)?,
            "countdown_adjusted_time": self.mem.read_u8(network_game_server_address + 1174)?,
        }))
    }

    fn get_global_variant(&mut self) -> Result<Value> {
        let global_variant_address = 0x2F90A8u64;
        let data = self.mem.read_bytes(global_variant_address, 0x68)?;
        let read_u32 = |offset: usize| -> u32 {
            u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
        };
        let read_f32 = |offset: usize| -> f32 {
            f32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
        };
        let game_type = read_u32(0x18);
        let player_settings = read_u32(0x20);
        let teamplay = read_u32(0x1C);
        let odd_man_out = read_u32(0x28);
        let objective_indicator = read_u32(0x24);
        let respawn_time_growth = read_u32(0x2C);
        let respawn_time = read_u32(0x30);
        let respawn_suicide_penalty = read_u32(0x34);
        let health = read_f32(0x3C);
        let score_limit = read_u32(0x40);
        let weapon_set = read_u32(0x44);
        let vehicle_set = read_u32(0x48);

        let mode_settings = match game_type {
            1 => {
                let ctf_flags = read_u32(0x4C);
                let single_flag_time = read_u32(0x50);
                json!({
                    "flags": ctf_flags,
                    "assault": (ctf_flags & (1 << 0)) != 0,
                    "flag_must_reset": (ctf_flags & (1 << 16)) != 0,
                    "flag_must_be_at_home": (ctf_flags & (1 << 24)) != 0,
                    "single_flag_time": single_flag_time,
                    "single_flag_time_seconds": single_flag_time as f64 / 30.0,
                })
            }
            2 => {
                let slayer_flags = read_u32(0x4C);
                json!({
                    "flags": slayer_flags,
                    "death_bonus": (slayer_flags & (1 << 0)) == 0,
                    "kill_penalty": (slayer_flags & (1 << 8)) == 0,
                    "kill_in_order": (slayer_flags & (1 << 16)) != 0,
                })
            }
            3 => {
                let random_ball = data[0x4C];
                let speed_with_ball = read_u32(0x50);
                let trait_with_ball = read_u32(0x54);
                let trait_without_ball = read_u32(0x58);
                let ball_type = read_u32(0x5C);
                json!({
                    "random_ball": random_ball,
                    "random_ball_name": enum_name(&OFF_ON_NAMES, random_ball as u32),
                    "speed_with_ball": speed_with_ball,
                    "speed_with_ball_name": enum_name(&SPEED_WITH_BALL_NAMES, speed_with_ball),
                    "trait_with_ball": trait_with_ball,
                    "trait_with_ball_name": enum_name(&TRAIT_WITH_BALL_NAMES, trait_with_ball),
                    "trait_without_ball": trait_without_ball,
                    "trait_without_ball_name": enum_name(&TRAIT_WITH_BALL_NAMES, trait_without_ball),
                    "ball_type": ball_type,
                    "ball_type_name": enum_name(&BALL_TYPE_NAMES, ball_type),
                    "ball_count": read_u32(0x60),
                })
            }
            4 => {
                let moving_hill = data[0x4C];
                json!({
                    "moving_hill": moving_hill,
                    "moving_hill_name": enum_name(&OFF_ON_NAMES, moving_hill as u32),
                })
            }
            5 => {
                let race_order = data[0x4C];
                let points_used = read_u32(0x50);
                json!({
                    "order": race_order,
                    "order_name": enum_name(&RACE_ORDER_NAMES, race_order as u32),
                    "points_used": points_used,
                    "points_used_name": enum_name(&RACE_POINTS_USED_NAMES, points_used),
                })
            }
            _ => Value::Object(Map::new()),
        };

        Ok(json!({
            "address": format!("{} -> {}", py_hex_u64(global_variant_address), py_hex_u64(self.mem.get_host_address(global_variant_address)?)),
            "name": read_utf16_from_bytes(&data[..24]),
            "game_type": game_type,
            "mode": enum_name(&GAMETYPE_NAMES, game_type),
            "teamplay": teamplay,
            "teams_enabled": teamplay != 0,
            "teamplay_name": enum_name(&OFF_ON_NAMES, teamplay),
            "player_settings": {
                "value": player_settings,
                "radar_enabled": (player_settings & (1 << 0)) != 0,
                "friend_on_hud": (player_settings & (1 << 1)) != 0,
                "infinite_grenades": (player_settings & (1 << 2)) != 0,
                "shields_disabled": (player_settings & (1 << 3)) != 0,
                "invisible": (player_settings & (1 << 4)) != 0,
                "generic_weapons": (player_settings & (1 << 5)) != 0,
                "enemies_not_on_radar": (player_settings & (1 << 6)) != 0,
            },
            "objective_indicator": objective_indicator,
            "objective_indicator_name": enum_name(&OBJECTIVE_INDICATOR_NAMES, objective_indicator),
            "odd_man_out": odd_man_out,
            "odd_man_out_name": enum_name(&OFF_ON_NAMES, odd_man_out),
            "respawn_time_growth": respawn_time_growth,
            "respawn_time_growth_seconds": respawn_time_growth as f64 / 30.0,
            "respawn_time": respawn_time,
            "respawn_time_seconds": respawn_time as f64 / 30.0,
            "respawn_suicide_penalty": respawn_suicide_penalty,
            "respawn_suicide_penalty_seconds": respawn_suicide_penalty as f64 / 30.0,
            "lives": read_u32(0x38),
            "health": health,
            "health_percent": health * 100.0,
            "score_limit": score_limit,
            "weapon_set": weapon_set,
            "weapon_set_name": enum_name(&WEAPON_SET_NAMES, weapon_set),
            "vehicle_set": vehicle_set,
            "vehicle_set_name": enum_name(&VEHICLE_SET_NAMES, vehicle_set),
            "mode_settings": mode_settings,
        }))
    }

    fn player_score_by_player_id(&mut self, player_id: u32, gametype: u32) -> Result<i32> {
        if gametype == 1 {
            return Ok(0);
        }
        let Some(base) = player_score_address_by_gametype(gametype) else {
            return Ok(0);
        };
        self.mem.read_i32(base + 4 * player_id as u64)
    }

    fn get_spawns(&mut self, cache_results: bool) -> Result<Vec<Value>> {
        if cache_results && !self.spawns_cache.is_empty() {
            return Ok(self.spawns_cache.clone());
        }
        let global_scenario_address = self.mem.read_u32(0x39BE5C)? as u64;
        let spawn_count = self.mem.read_u32(global_scenario_address + 852)?;
        let first_spawn_address = self.mem.read_u32(global_scenario_address + 856)? as u64;
        let mut spawns = Vec::new();
        if spawn_count > 0 {
            for spawn_index in 0..spawn_count as u64 {
                let spawn_address = first_spawn_address + 52 * spawn_index;
                spawns.push(json!({
                    "address": format!("{} -> {}", py_hex_u64(spawn_address), py_hex_u64(self.mem.get_host_address(spawn_address)?)),
                    "spawn_id": spawn_index,
                    "x": self.mem.read_f32(spawn_address)?,
                    "y": self.mem.read_f32(spawn_address + 4)?,
                    "z": self.mem.read_f32(spawn_address + 8)?,
                    "facing": self.mem.read_f32(spawn_address + 12)?,
                    "team_index": self.mem.read_u8(spawn_address + 16)?,
                    "bsp_index": self.mem.read_u8(spawn_address + 17)?,
                    "unk0": py_hex_u32(self.mem.read_u16(spawn_address + 18)? as u32),
                    "gametypes": [
                        self.mem.read_u8(spawn_address + 20)?,
                        self.mem.read_u8(spawn_address + 21)?,
                        self.mem.read_u8(spawn_address + 22)?,
                        self.mem.read_u8(spawn_address + 23)?,
                    ],
                }));
            }
        }
        if cache_results {
            self.spawns_cache = spawns.clone();
        }
        Ok(spawns)
    }

    fn get_items(&mut self, cache_results: bool) -> Result<Vec<Value>> {
        if cache_results && !self.items_cache.is_empty() {
            return Ok(self.items_cache.clone());
        }
        let global_scenario_address = self.mem.read_u32(0x39BE5C)? as u64;
        let item_count = self.mem.read_i32(global_scenario_address + 900)?;
        let first_item_address = self.mem.read_u32(global_scenario_address + 904)? as u64;
        let mut items = Vec::new();
        if item_count > 0 {
            for item_index in 0..item_count as u64 {
                let item_address = first_item_address + 144 * item_index;
                let _unknown_item_attribute = self.mem.read_i16(item_address + 0xE)?;
                let tag_index = self.mem.read_i32(item_address + 0x5C)?;
                if tag_index != -1 {
                    let tag_id = (tag_index as u32) & 0xFFFF;
                    let tag_instance_address =
                        self.globals.global_tag_instances_address as u64 + 32 * tag_id as u64;
                    let tag_name_ptr = self.mem.read_i32(tag_instance_address + 0x10)? as u32 as u64;
                    let tag_definition = self.mem.read_i32(tag_instance_address + 0x14)? as u32 as u64;
                    items.push(json!({
                        "address": format!("{} -> {}", py_hex_u64(item_address), py_hex_u64(self.mem.get_host_address(item_address)?)),
                        "tag_id": tag_id,
                        "tag_name": self.mem.read_string(tag_name_ptr, 128)?,
                        "item_spawn_interval": self.mem.read_i16(tag_definition + 0xC)?,
                        "item_game_type": self.mem.read_u8(item_address + 0x4)?,
                        "item_x": self.mem.read_f32(item_address + 0x40)?,
                        "item_y": self.mem.read_f32(item_address + 0x44)?,
                        "item_z": self.mem.read_f32(item_address + 0x48)?,
                    }));
                }
            }
        }
        if cache_results {
            self.items_cache = items.clone();
        }
        Ok(items)
    }

    fn get_objects(&mut self) -> Result<Vec<Value>> {
        let mut objects = Vec::new();
        let object_header_datum_array = self.mem.read_u32(0x2FC6AC)?;
        let object_header_datum_array_total_count =
            self.mem.read_u16(object_header_datum_array as u64 + 0x2E)?;
        let object_header_datum_array_first_element_address =
            self.mem.read_u32(object_header_datum_array as u64 + 0x34)?;
        let item_datum_size = self.mem.read_u16(0x1FC380)?;

        for i in 0..object_header_datum_array_total_count as u64 {
            let header_address = object_header_datum_array_first_element_address as u64 + 12 * i;
            let object_address = self.mem.read_u32(header_address + 8)?;
            if object_address == 0 {
                continue;
            }
            let object_address_u64 = object_address as u64;
            let tag_index = self.mem.read_i16(object_address_u64)?;
            let tag_name_ptr = self.mem.read_u32(
                (32i64 * tag_index as i64 + self.globals.global_tag_instances_address as i64 + 0x10)
                    as u64,
            )?;
            let tag_name = self.mem.read_string(tag_name_ptr as u64, 128)?;
            let object_type = self.mem.read_u8(object_address_u64 + 0x64)?;
            let object_type_string = self.object_string_from_type(object_type)?;
            let mut object = Map::new();
            object.insert("object_id".to_string(), json!(i));
            object.insert(
                "address".to_string(),
                Value::String(format!(
                    "{} -> {}",
                    py_hex_u32(object_address),
                    py_hex_u64(self.mem.get_host_address(object_address_u64)?)
                )),
            );
            object.insert(
                "header_data".to_string(),
                Value::Array(
                    self.mem
                        .formatted_bytes(header_address, 12, 32)?
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            );
            object.insert(
                "flags".to_string(),
                Value::String(py_hex_u32(self.mem.read_u32(object_address_u64 + 0x4)?)),
            );
            for (name, offset) in [
                ("x", 0xC),
                ("y", 0x10),
                ("z", 0x14),
                ("vel_x", 0x18),
                ("vel_y", 0x1C),
                ("vel_z", 0x20),
                ("forward_x", 0x24),
                ("forward_y", 0x28),
                ("forward_z", 0x2C),
                ("up_x", 0x30),
                ("up_y", 0x34),
                ("up_z", 0x38),
                ("ang_vel_x", 0x3C),
                ("ang_vel_y", 0x40),
                ("ang_vel_z", 0x44),
            ] {
                object.insert(name.to_string(), f32_value(self.mem.read_f32(object_address_u64 + offset)?));
            }
            object.insert("time_existing".to_string(), json!(self.mem.read_i16(object_address_u64 + 0x6C)?));
            object.insert("unk_damage_1".to_string(), json!(self.mem.read_i16(object_address_u64 + 0x68)?));
            object.insert(
                "owner_unit_ref".to_string(),
                Value::String(py_hex_u32(self.mem.read_u32(object_address_u64 + 0x70)?)),
            );
            object.insert(
                "owner_object_ref".to_string(),
                Value::String(py_hex_u32(self.mem.read_u32(object_address_u64 + 0x74)?)),
            );
            object.insert(
                "parent_ref".to_string(),
                Value::String(py_hex_u32(self.mem.read_u32(object_address_u64 + 0xCC)?)),
            );
            object.insert(
                "ultimate_parent".to_string(),
                Value::String(py_hex_u32(self.mem.read_u32(object_address_u64 + 0x1E4)?)),
            );
            object.insert("state_flags".to_string(), json!(self.mem.read_u8(object_address_u64 + 0x1A4)?));
            object.insert("drop_time".to_string(), json!(self.mem.read_u32(object_address_u64 + 0x1B4)?));
            object.insert("object_type".to_string(), json!(object_type));
            object.insert("object_type_string".to_string(), Value::String(object_type_string.clone()));
            object.insert("tag_name".to_string(), Value::String(tag_name));

            if object_type_string == "projectile" {
                let projectile_address = object_address_u64 + item_datum_size as u64;
                object.insert(
                    "type_specific_data".to_string(),
                    json!({
                        "flags": self.mem.read_u32(projectile_address)?,
                        "address": format!("{} -> {}", py_hex_u64(projectile_address), py_hex_u64(self.mem.get_host_address(projectile_address)?)),
                        "action": self.mem.read_i16(projectile_address + 0x4)?,
                        "hit_material_type": self.mem.read_i16(projectile_address + 0x6)?,
                        "ignore_object_index": self.mem.read_i32(projectile_address + 0x8)?,
                        "target_object_index": self.mem.read_i32(projectile_address + 0x1C)?,
                        "detonation_timer": self.mem.read_f32(projectile_address + 0x14)?,
                        "detonation_timer_delta": self.mem.read_f32(projectile_address + 0x18)?,
                        "arming_time": self.mem.read_f32(projectile_address + 0x1C)?,
                        "arming_time_delta": self.mem.read_f32(projectile_address + 0x20)?,
                        "distance_traveled": self.mem.read_f32(projectile_address + 0x24)?,
                        "deceleration_timer": self.mem.read_f32(projectile_address + 0x28)?,
                        "deceleration_timer_delta": self.mem.read_f32(projectile_address + 0x2C)?,
                        "deceleration": self.mem.read_f32(projectile_address + 0x30)?,
                        "maximum_damage_distance": self.mem.read_f32(projectile_address + 0x34)?,
                        "rotation_axis_x": self.mem.read_f32(projectile_address + 0x3C)?,
                        "rotation_axis_y": self.mem.read_f32(projectile_address + 0x40)?,
                        "rotation_axis_z": self.mem.read_f32(projectile_address + 0x44)?,
                        "rotation_sine": self.mem.read_f32(projectile_address + 0x48)?,
                        "rotation_cosine": self.mem.read_f32(projectile_address + 0x4C)?,
                    }),
                );
            }
            objects.push(Value::Object(object));
        }
        Ok(objects)
    }

    fn arrange_objects_by_type(&self, objects: &[Value]) -> Value {
        let mut indexes_by_type: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut ids_by_type: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut projectiles_by_unit_id: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for (index, object) in objects.iter().enumerate() {
            let object_type = object
                .get("object_type_string")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            indexes_by_type.entry(object_type.clone()).or_default().push(json!(index));
            if let Some(object_id) = object.get("object_id") {
                ids_by_type.entry(object_type.clone()).or_default().push(object_id.clone());
            }
            if object_type == "projectile" {
                let owner = object
                    .get("owner_unit_ref")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                projectiles_by_unit_id.entry(owner).or_default().push(json!(index));
            }
        }
        json!({
            "object_indexes_by_type": indexes_by_type,
            "object_ids_by_type": ids_by_type,
            "projectiles_by_unit_id": projectiles_by_unit_id,
        })
    }

    fn get_map_resolution_inputs(&self, game_info: &Value) -> Value {
        let map_info = game_info.get("map_info").and_then(Value::as_object);
        let mut spawn_records = Vec::new();
        if let Some(spawns) = game_info.get("spawns").and_then(Value::as_array) {
            for spawn in spawns {
                let mut gametypes = spawn
                    .get("gametypes")
                    .and_then(Value::as_array)
                    .map(|items| {
                        let mut values = items
                            .iter()
                            .filter_map(|item| item.as_i64().or_else(|| item.as_u64().map(|v| v as i64)))
                            .collect::<Vec<_>>();
                        values.sort();
                        values
                    })
                    .unwrap_or_default();
                gametypes.sort();
                spawn_records.push(json!({
                    "spawn_id": canonical_int(spawn.get("spawn_id")),
                    "x": canonical_float(spawn.get("x")),
                    "y": canonical_float(spawn.get("y")),
                    "z": canonical_float(spawn.get("z")),
                    "facing": canonical_float(spawn.get("facing")),
                    "team_index": canonical_int(spawn.get("team_index")),
                    "gametypes": gametypes,
                }));
            }
        }
        spawn_records.sort_by_key(|spawn| {
            spawn
                .get("spawn_id")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX)
        });
        json!({
            "map_engine_name": optional_string(game_info.get("multiplayer_map_name")),
            "map_info": {
                "scenario_name": optional_string(map_info.and_then(|map| map.get("scenario_name"))),
                "build_version": optional_string(map_info.and_then(|map| map.get("build_version"))),
                "cache_version": canonical_int(map_info.and_then(|map| map.get("cache_version"))),
                "checksum": canonical_int(map_info.and_then(|map| map.get("checksum"))),
            },
            "game_type": canonical_int(game_info.get("game_type")),
            "variant": canonical_int(game_info.get("variant")),
            "game_engine_has_teams": truthy(game_info.get("game_engine_has_teams")),
            "spawn_points": spawn_records,
        })
    }
}

fn calculate_spawn_hash(map_resolution_inputs: &Value) -> Result<String> {
    let sorted = sort_json_value(map_resolution_inputs);
    let bytes = serde_json::to_vec(&sorted)?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{digest:x}"))
}

const GAMETYPE_NAMES: &[(u32, &str)] = &[
    (0, "none"),
    (1, "ctf"),
    (2, "slayer"),
    (3, "oddball"),
    (4, "king"),
    (5, "race"),
    (6, "terminator"),
    (7, "stub"),
];

const OFF_ON_NAMES: &[(u32, &str)] = &[(0, "off"), (1, "on")];

const OBJECTIVE_INDICATOR_NAMES: &[(u32, &str)] = &[
    (0, "motion tracker"),
    (1, "nav point"),
    (2, "none"),
];

const WEAPON_SET_NAMES: &[(u32, &str)] = &[
    (0, "default"),
    (1, "pistols"),
    (2, "rifles"),
    (3, "plasma rifles"),
    (4, "sniper"),
    (5, "no sniper"),
    (6, "rocket launchers"),
    (7, "shotguns"),
    (8, "short range"),
    (9, "human"),
    (10, "covenant"),
    (11, "classic"),
    (12, "heavy weapons"),
];

const VEHICLE_SET_NAMES: &[(u32, &str)] = &[
    (0, "all"),
    (1, "none"),
    (2, "warthog"),
    (3, "ghost"),
    (4, "scorpion"),
];

const SPEED_WITH_BALL_NAMES: &[(u32, &str)] = &[
    (0, "slow"),
    (1, "normal"),
    (2, "fast"),
];

const TRAIT_WITH_BALL_NAMES: &[(u32, &str)] = &[
    (0, "none"),
    (1, "invisible"),
    (2, "extra damage"),
    (3, "damage resistance"),
];

const BALL_TYPE_NAMES: &[(u32, &str)] = &[
    (0, "normal"),
    (1, "reverse tag"),
    (2, "juggernaut"),
];

const RACE_ORDER_NAMES: &[(u32, &str)] = &[
    (0, "normal"),
    (1, "any order"),
    (2, "rally"),
];

const RACE_POINTS_USED_NAMES: &[(u32, &str)] = &[
    (0, "minimum"),
    (1, "maximum"),
    (2, "sum"),
];

fn enum_name(names: &[(u32, &'static str)], value: u32) -> String {
    names
        .iter()
        .find_map(|(key, name)| (*key == value).then_some((*name).to_string()))
        .unwrap_or_else(|| format!("unknown {value}"))
}

fn player_score_address_by_gametype(gametype: u32) -> Option<u64> {
    match gametype {
        2 => Some(0x276710 + 64),
        3 => Some(0x27653C + 64),
        4 => Some(0x2762D8 + 64),
        5 => Some(0x2766C8 + 64),
        _ => None,
    }
}

fn optional_string(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(value)) if !value.is_empty() => Value::String(value.clone()),
        Some(value) if !value.is_null() => {
            let text = value
                .as_str()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| value.to_string());
            if text.is_empty() {
                Value::Null
            } else {
                Value::String(text)
            }
        }
        _ => Value::Null,
    }
}

fn canonical_int(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Number(number)) => {
            if let Some(value) = number.as_i64() {
                json!(value)
            } else if let Some(value) = number.as_u64() {
                json!(value)
            } else {
                Value::Null
            }
        }
        Some(Value::String(value)) if !value.is_empty() => value
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn canonical_float(value: Option<&Value>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    let number = value
        .as_f64()
        .or_else(|| value.as_i64().map(|value| value as f64))
        .or_else(|| value.as_u64().map(|value| value as f64))
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()));
    match number {
        Some(value) if value.is_finite() => f64_value((value * 1_000_000.0).round() / 1_000_000.0),
        _ => Value::Null,
    }
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => {
            number.as_i64().unwrap_or(0) != 0 || number.as_u64().unwrap_or(0) != 0
        }
        Some(Value::String(value)) => !value.is_empty(),
        _ => false,
    }
}

fn sort_json_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted_keys = map.keys().collect::<Vec<_>>();
            sorted_keys.sort();
            let mut sorted = Map::new();
            for key in sorted_keys {
                sorted.insert(key.clone(), sort_json_value(&map[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_json_value).collect()),
        _ => value.clone(),
    }
}

fn read_utf16_from_bytes(bytes: &[u8]) -> String {
    let mut words = Vec::new();
    for chunk in bytes.chunks_exact(2) {
        let word = u16::from_le_bytes([chunk[0], chunk[1]]);
        if word == 0 {
            break;
        }
        words.push(word);
    }
    String::from_utf16_lossy(&words)
}
