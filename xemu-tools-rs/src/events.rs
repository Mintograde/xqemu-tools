use crate::util::{as_f64, as_i64, get_path, json_number_from_f64_or_i64};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct GameMeta {
    pub start_time: Option<String>,
    pub players: BTreeMap<usize, PlayerMeta>,
}

#[derive(Debug, Clone, Default)]
pub struct PlayerMeta {
    shots_by_weapon: BTreeMap<String, i64>,
    damage_to_player: BTreeMap<String, f64>,
    damage_from_player: BTreeMap<String, f64>,
    kills_by_player: BTreeMap<String, i64>,
    deaths_by_player: BTreeMap<String, i64>,
    shots_by_tick: BTreeMap<String, i64>,
    kills_by_tick: BTreeMap<String, i64>,
    deaths_by_tick: BTreeMap<String, i64>,
    assists_by_tick: BTreeMap<String, i64>,
    damage_dealt_by_tick: BTreeMap<String, f64>,
    damage_dealt: f64,
    damage_received_by_tick: BTreeMap<String, f64>,
    damage_received: f64,
    camo_by_tick: BTreeMap<String, i64>,
    camo_count: i64,
    overshield_by_tick: BTreeMap<String, i64>,
    overshield_count: i64,
    active_projectiles: Vec<Value>,
}

pub struct ExtractResult {
    pub events: Vec<String>,
    pub clear_caches: bool,
}

impl GameMeta {
    pub fn initialize_players(&mut self, game_info: &Value) {
        self.players.clear();
        if let Some(players) = game_info.get("players").and_then(Value::as_array) {
            for player in players {
                if let Some(index) = player.get("player_index").and_then(as_i64) {
                    self.players.insert(index as usize, PlayerMeta::default());
                }
            }
        }
    }

    pub fn ensure_players(&mut self, game_info: &Value) {
        if let Some(players) = game_info.get("players").and_then(Value::as_array) {
            for player in players {
                if let Some(index) = player.get("player_index").and_then(as_i64) {
                    self.players.entry(index as usize).or_default();
                }
            }
        }
    }

    pub fn to_value(&self) -> Value {
        let mut root = Map::new();
        root.insert(
            "start_time".to_string(),
            self.start_time
                .as_ref()
                .map(|value| Value::String(value.clone()))
                .unwrap_or(Value::Null),
        );

        let mut players = Map::new();
        for (index, meta) in &self.players {
            players.insert(index.to_string(), meta.to_value());
        }
        root.insert("players".to_string(), Value::Object(players));
        Value::Object(root)
    }

    pub fn extract_events(&mut self, old_game_info: &Value, new_game_info: &mut Value) -> ExtractResult {
        let mut events = Vec::new();
        let mut clear_caches = false;
        let game_time = get_path(new_game_info, &["game_time_info", "game_time"])
            .and_then(as_i64)
            .unwrap_or(0);
        let game_time_key = game_time.to_string();

        if self.players.is_empty() {
            self.initialize_players(new_game_info);
        } else {
            self.ensure_players(new_game_info);
        }

        let old_engine_running = bool_field(old_game_info, "game_engine_running");
        let new_engine_running = bool_field(new_game_info, "game_engine_running");
        let old_can_score = bool_field(old_game_info, "game_engine_can_score");
        let new_can_score = bool_field(new_game_info, "game_engine_can_score");

        if !old_engine_running && new_engine_running {
            let map_name = string_field(new_game_info, "multiplayer_map_name");
            events.push(format!("{game_time}: New game started on {map_name}"));
            self.start_time = new_game_info
                .get("current_time")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            self.initialize_players(new_game_info);
            clear_caches = true;
        }

        if new_can_score && new_game_info.get("objects").is_some() {
            let new_projectiles = projectile_ids(new_game_info);
            let old_projectiles = projectile_ids(old_game_info);
            for object_id in &new_projectiles {
                if !old_projectiles.contains(object_id) {
                    // Python currently detects these but does not emit an event.
                }
            }
            for object_id in &old_projectiles {
                if !new_projectiles.contains(object_id) {
                    // Python currently detects these but does not emit an event.
                }
            }
        }

        if new_can_score {
            if let (Some(old_players), Some(new_players)) = (
                old_game_info.get("players").and_then(Value::as_array),
                new_game_info.get("players").and_then(Value::as_array),
            ) {
                if old_players.len() == new_players.len() {
                    for (old_player, new_player) in old_players.iter().zip(new_players) {
                        let Some(old_data) = old_player.get("player_object_data") else {
                            continue;
                        };
                        let Some(new_data) = new_player.get("player_object_data") else {
                            continue;
                        };
                        if !object_nonempty(old_data) || !object_nonempty(new_data) {
                            continue;
                        }

                        if let (Some(old_weapons), Some(new_weapons)) = (
                            old_data.get("weapons").and_then(Value::as_array),
                            new_data.get("weapons").and_then(Value::as_array),
                        ) {
                            for (old_weapon, new_weapon) in old_weapons.iter().zip(new_weapons) {
                                if old_weapon.get("object_id") != new_weapon.get("object_id") {
                                    continue;
                                }
                                let is_energy_weapon = bool_field(new_weapon, "is_energy_weapon");
                                let old_ammo = if is_energy_weapon {
                                    number_field(old_weapon, "charge_amount")
                                } else {
                                    number_field(old_weapon, "magazine_ammo_count")
                                };
                                let new_ammo = if is_energy_weapon {
                                    number_field(new_weapon, "charge_amount")
                                } else {
                                    number_field(new_weapon, "magazine_ammo_count")
                                };
                                if old_ammo > new_ammo {
                                    let player_index = int_field(new_player, "player_index") as usize;
                                    let tag_name = string_field(new_weapon, "tag_name");
                                    let amount = if is_energy_weapon {
                                        1
                                    } else {
                                        (old_ammo - new_ammo) as i64
                                    };
                                    let meta = self.players.entry(player_index).or_default();
                                    *meta.shots_by_weapon.entry(tag_name).or_default() += amount;
                                    *meta.shots_by_tick.entry(game_time_key.clone()).or_default() += amount;
                                }
                            }
                        }

                        let old_primary = int_field(old_data, "primary_nades");
                        let new_primary = int_field(new_data, "primary_nades");
                        if old_primary > new_primary {
                            events.push(format!(
                                "{game_time}: {} threw frag grenade ({old_primary} -> {new_primary})",
                                string_field(new_player, "name")
                            ));
                        }
                        let old_secondary = int_field(old_data, "secondary_nades");
                        let new_secondary = int_field(new_data, "secondary_nades");
                        if old_secondary > new_secondary {
                            events.push(format!(
                                "{game_time}: {} threw plasma grenade ({old_secondary} -> {new_secondary})",
                                string_field(new_player, "name")
                            ));
                        }
                    }
                }
            }
        }

        if new_can_score {
            self.extract_damage_events(old_game_info, new_game_info, &mut events, game_time, &game_time_key);
        }

        if old_engine_running && new_engine_running {
            self.extract_player_stat_events(old_game_info, new_game_info, &mut events, game_time, &game_time_key);
        }

        if new_can_score {
            self.extract_spawn_events(old_game_info, new_game_info, &mut events, game_time);
        }

        if old_can_score && !new_can_score {
            let map_name = string_field(new_game_info, "multiplayer_map_name");
            events.push(format!("{game_time}: Game ended on {map_name}"));
            self.start_time = None;
            if let Some(map) = new_game_info.as_object_mut() {
                map.insert("game_ended_this_tick".to_string(), Value::Bool(true));
                if let Some(old_game_id) = old_game_info.get("game_id") {
                    map.insert("game_id".to_string(), old_game_id.clone());
                }
            }
        }

        if let Some(map) = new_game_info.as_object_mut() {
            map.insert("game_meta".to_string(), self.to_value());
        }

        ExtractResult { events, clear_caches }
    }

    fn extract_damage_events(
        &mut self,
        old_game_info: &Value,
        new_game_info: &Value,
        events: &mut Vec<String>,
        game_time: i64,
        game_time_key: &str,
    ) {
        let Some(new_damage_counts) = new_game_info.get("damage_counts").and_then(Value::as_object) else {
            return;
        };
        let old_damage_counts = old_game_info
            .get("damage_counts")
            .and_then(Value::as_object);

        for (damage_dealer_key, damage_receivers) in new_damage_counts {
            let Ok(damage_dealer) = damage_dealer_key.parse::<usize>() else {
                continue;
            };
            let damage_dealer_name = player_name(new_game_info, damage_dealer);
            let Some(damage_receivers) = damage_receivers.as_object() else {
                continue;
            };
            let old_dealer_counts = old_damage_counts
                .and_then(|counts| counts.get(damage_dealer_key))
                .and_then(Value::as_object);

            for (damage_receiver_key, new_amount_value) in damage_receivers {
                let Ok(damage_receiver) = damage_receiver_key.parse::<usize>() else {
                    continue;
                };
                let damage_receiver_name = player_name(new_game_info, damage_receiver);
                let new_amount = as_f64(new_amount_value).unwrap_or(0.0);
                let old_amount = old_dealer_counts
                    .and_then(|counts| counts.get(damage_receiver_key))
                    .and_then(as_f64);
                let delta = match old_amount {
                    Some(old_amount) if new_amount > old_amount => new_amount - old_amount,
                    Some(_) => continue,
                    None => new_amount,
                };
                events.push(format!(
                    "{game_time}: {damage_dealer_name} damaged {damage_receiver_name} for {delta}"
                ));
                let dealer_meta = self.players.entry(damage_dealer).or_default();
                add_f64(&mut dealer_meta.damage_dealt_by_tick, game_time_key, delta);
                add_f64(&mut dealer_meta.damage_to_player, &damage_receiver.to_string(), delta);
                dealer_meta.damage_dealt += delta;

                let receiver_meta = self.players.entry(damage_receiver).or_default();
                add_f64(&mut receiver_meta.damage_received_by_tick, game_time_key, delta);
                add_f64(&mut receiver_meta.damage_from_player, &damage_dealer.to_string(), delta);
                receiver_meta.damage_received += delta;
            }
        }
    }

    fn extract_player_stat_events(
        &mut self,
        old_game_info: &Value,
        new_game_info: &Value,
        events: &mut Vec<String>,
        game_time: i64,
        game_time_key: &str,
    ) {
        let (Some(old_players), Some(new_players)) = (
            old_game_info.get("players").and_then(Value::as_array),
            new_game_info.get("players").and_then(Value::as_array),
        ) else {
            return;
        };
        if old_players.len() != new_players.len() {
            return;
        }

        for (old_player, new_player) in old_players.iter().zip(new_players) {
            let player_index = int_field(new_player, "player_index") as usize;
            let player_name = string_field(new_player, "name");
            let meta = self.players.entry(player_index).or_default();

            let kills = int_field(new_player, "kills");
            let old_kills = int_field(old_player, "kills");
            if kills > old_kills {
                events.push(format!("{game_time}: {player_name} got a kill ({kills})"));
                *meta.kills_by_tick.entry(game_time_key.to_string()).or_default() += kills - old_kills;
            }

            let deaths = int_field(new_player, "deaths");
            let old_deaths = int_field(old_player, "deaths");
            if deaths > old_deaths {
                events.push(format!("{game_time}: {player_name} died ({deaths})"));
                *meta.deaths_by_tick.entry(game_time_key.to_string()).or_default() += deaths - old_deaths;
            }

            let assists = int_field(new_player, "assists");
            let old_assists = int_field(old_player, "assists");
            if assists > old_assists {
                events.push(format!("{game_time}: {player_name} got an assist ({assists})"));
                *meta.assists_by_tick.entry(game_time_key.to_string()).or_default() += assists - old_assists;
            }

            let has_camo = get_path(new_player, &["derived_stats", "has_camo"])
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let old_has_camo = get_path(old_player, &["derived_stats", "has_camo"])
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if has_camo && !old_has_camo {
                events.push(format!("{game_time}: {player_name} picked up camo"));
                *meta.camo_by_tick.entry(game_time_key.to_string()).or_default() += 1;
                meta.camo_count += 1;
            }
            if !has_camo && old_has_camo {
                events.push(format!("{game_time}: {player_name} lost camo"));
            }

            let has_overshield = get_path(new_player, &["derived_stats", "has_overshield"])
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let old_has_overshield = get_path(old_player, &["derived_stats", "has_overshield"])
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if has_overshield && !old_has_overshield {
                events.push(format!("{game_time}: {player_name} picked up overshield"));
                *meta.overshield_by_tick.entry(game_time_key.to_string()).or_default() += 1;
                meta.overshield_count += 1;
            }
            if !has_overshield && old_has_overshield {
                events.push(format!("{game_time}: {player_name} lost overshield"));
            }
        }
    }

    fn extract_spawn_events(
        &mut self,
        old_game_info: &Value,
        new_game_info: &Value,
        events: &mut Vec<String>,
        game_time: i64,
    ) {
        let Some(new_players) = new_game_info.get("players").and_then(Value::as_array) else {
            return;
        };
        let Some(spawns) = new_game_info.get("spawns").and_then(Value::as_array) else {
            return;
        };
        if spawns.is_empty() {
            return;
        }
        let old_players = old_game_info.get("players").and_then(Value::as_array);
        let game_type = int_field(new_game_info, "game_type") as i32;

        for (index, new_player) in new_players.iter().enumerate() {
            let old_player = old_players.and_then(|players| players.get(index));
            let old_has_object = old_player
                .and_then(|player| player.get("player_object_data"))
                .map(object_nonempty)
                .unwrap_or(false);
            let Some(new_data) = new_player.get("player_object_data") else {
                continue;
            };
            if old_has_object || !object_nonempty(new_data) {
                continue;
            }

            let player_x = number_field(new_data, "x");
            let player_y = number_field(new_data, "y");
            let player_z = number_field(new_data, "z");
            let mut spawn_found = false;
            for spawn in spawns {
                let spawn_x = number_field(spawn, "x");
                let spawn_y = number_field(spawn, "y");
                let spawn_z = number_field(spawn, "z");
                let d = distance((player_x, player_y, player_z), (spawn_x, spawn_y, spawn_z));
                let gametypes = spawn
                    .get("gametypes")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(as_i64)
                            .map(|value| value as i32)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if matches_gametype(game_type, &gametypes) && d <= 0.2 {
                    events.push(format!(
                        "{game_time}: {} spawned at spawn id {}",
                        string_field(new_player, "name"),
                        int_field(spawn, "spawn_id")
                    ));
                    spawn_found = true;
                    break;
                }
            }
            if !spawn_found {
                events.push(format!(
                    "{game_time}: {} spawned at an unknown spawn id ({player_x}, {player_y}, {player_z})",
                    string_field(new_player, "name")
                ));
            }
        }
    }
}

impl PlayerMeta {
    fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("shots_by_weapon".to_string(), i64_map_to_value(&self.shots_by_weapon));
        map.insert("damage_to_player".to_string(), f64_map_to_value(&self.damage_to_player));
        map.insert("damage_from_player".to_string(), f64_map_to_value(&self.damage_from_player));
        map.insert("kills_by_player".to_string(), i64_map_to_value(&self.kills_by_player));
        map.insert("deaths_by_player".to_string(), i64_map_to_value(&self.deaths_by_player));
        map.insert("shots_by_tick".to_string(), i64_map_to_value(&self.shots_by_tick));
        map.insert("kills_by_tick".to_string(), i64_map_to_value(&self.kills_by_tick));
        map.insert("deaths_by_tick".to_string(), i64_map_to_value(&self.deaths_by_tick));
        map.insert("assists_by_tick".to_string(), i64_map_to_value(&self.assists_by_tick));
        map.insert(
            "damage_dealt_by_tick".to_string(),
            f64_map_to_value(&self.damage_dealt_by_tick),
        );
        map.insert(
            "damage_dealt".to_string(),
            json_number_from_f64_or_i64(self.damage_dealt),
        );
        map.insert(
            "damage_received_by_tick".to_string(),
            f64_map_to_value(&self.damage_received_by_tick),
        );
        map.insert(
            "damage_received".to_string(),
            json_number_from_f64_or_i64(self.damage_received),
        );
        map.insert("camo_by_tick".to_string(), i64_map_to_value(&self.camo_by_tick));
        map.insert("camo_count".to_string(), json!(self.camo_count));
        map.insert(
            "overshield_by_tick".to_string(),
            i64_map_to_value(&self.overshield_by_tick),
        );
        map.insert("overshield_count".to_string(), json!(self.overshield_count));
        map.insert(
            "active_projectiles".to_string(),
            Value::Array(self.active_projectiles.clone()),
        );
        Value::Object(map)
    }
}

fn i64_map_to_value(input: &BTreeMap<String, i64>) -> Value {
    let mut map = Map::new();
    for (key, value) in input {
        map.insert(key.clone(), json!(*value));
    }
    Value::Object(map)
}

fn f64_map_to_value(input: &BTreeMap<String, f64>) -> Value {
    let mut map = Map::new();
    for (key, value) in input {
        map.insert(key.clone(), json_number_from_f64_or_i64(*value));
    }
    Value::Object(map)
}

fn add_f64(map: &mut BTreeMap<String, f64>, key: &str, amount: f64) {
    *map.entry(key.to_string()).or_default() += amount;
}

fn bool_field(value: &Value, field: &str) -> bool {
    value.get(field).and_then(Value::as_bool).unwrap_or(false)
}

fn int_field(value: &Value, field: &str) -> i64 {
    value.get(field).and_then(as_i64).unwrap_or(0)
}

fn number_field(value: &Value, field: &str) -> f64 {
    value.get(field).and_then(as_f64).unwrap_or(0.0)
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn object_nonempty(value: &Value) -> bool {
    value.as_object().map(|object| !object.is_empty()).unwrap_or(false)
}

fn player_name(game_info: &Value, player_index: usize) -> String {
    game_info
        .get("players")
        .and_then(Value::as_array)
        .and_then(|players| players.get(player_index))
        .and_then(|player| player.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn projectile_ids(game_info: &Value) -> Vec<i64> {
    get_path(game_info, &["objects_meta", "object_ids_by_type", "projectile"])
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(as_i64).collect())
        .unwrap_or_default()
}

fn distance(p1: (f64, f64, f64), p2: (f64, f64, f64)) -> f64 {
    let (x1, y1, z1) = p1;
    let (x2, y2, z2) = p2;
    (((x2 - x1).powi(2)) + ((y2 - y1).powi(2)) + ((z2 - z1).powi(2))).sqrt()
}

fn matches_gametype(current_gametype: i32, gametype_list: &[i32]) -> bool {
    gametype_list.iter().any(|gametype| {
        current_gametype == *gametype
            || *gametype == 12
            || (*gametype == 13 && current_gametype != 1)
            || (*gametype == 14 && current_gametype != 1 && current_gametype != 5)
    })
}
