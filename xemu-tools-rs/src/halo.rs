use serde::{Deserialize, Serialize};
use anyhow::Result;
use crate::memory::MemoryReader;
use std::collections::HashMap;
use chrono::Local;

// --- Constants & Offsets ---
const GAME_STATE_BASE_PTR: u32 = 0x2E2D14;
const GAME_STATE_SIZE_PTR: u32 = 0x32E4A;
const GLOBAL_SCENARIO_PTR: u32 = 0x39BE5C;
const GAME_TIME_GLOBALS_PTR: u32 = 0x2F8CA0;
const PLAYER_DATUM_ARRAY: u32 = 0x2FAD28;
const OBJECT_HEADER_DATUM_ARRAY: u32 = 0x2FC6AC;
const OBJECT_TYPE_DEFINITIONS: u32 = 0x1FC0D0;
const GLOBAL_TAG_INSTANCES: u32 = 0x39CE24;
const FOG_PARAMS: u32 = 0x2FC8A8;
const NETWORK_GAME_CLIENT: u32 = 0x2FB180;

const TEAM_SCORES_CTF: u32 = 0x2762B4;
const TEAM_SCORES_SLAYER: u32 = 0x276710;
const TEAM_SCORES_ODDBALL: u32 = 0x27653C;
const TEAM_SCORES_KING: u32 = 0x2762D8;
const TEAM_SCORES_RACE: u32 = 0x2766C8;

const INPUT_ABSTRACTION_GLOBALS: u32 = 0x2E4600;

// --- Structs ---

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PerformanceMetrics {
    pub loop_time: f64,
    pub game_info_time: f64,
    pub post_steps_ms: f64,
    pub memory_mbytes: f64,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct GameTimeInfo {
    pub game_time: u32,
    pub game_time_elapsed: u32,
    pub real_time_elapsed: String,
    pub game_time_initialized: u8,
    pub game_time_active: u8,
    pub game_time_paused: u8,
    pub game_time_speed: f32,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Spawn {
    pub spawn_id: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub facing: f32,
    pub team_index: u8,
    pub gametypes: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Item {
    pub tag_name: String,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct FogInfo {
    pub color_r: f32,
    pub color_g: f32,
    pub color_b: f32,
    pub max_density: f32,
    pub atmo_min_dist: f32,
    pub atmo_max_dist: f32,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct TeamScores {
    pub ctf_team_score: (u32, u32),
    pub ctf_score_limit: u32,
    pub slayer_team_score: (u32, u32),
    pub slayer_score_limit: u32,
    pub oddball_team_score: (u32, u32),
    pub oddball_score_limit: u32,
    pub king_team_score: (u32, u32),
    pub race_team_score: (u32, u32),
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct GameObject {
    pub object_id: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub object_type_string: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ObjectsMeta {
    pub object_indexes_by_type: HashMap<String, Vec<usize>>,
    pub object_ids_by_type: HashMap<String, Vec<u32>>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct WeaponData {
    pub tag_name: String,
    pub magazine_ammo_count: i16,
    pub charge_amount: f32,
    pub is_energy_weapon: bool,
    pub object_id: u16,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct PlayerObjectData {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub x_vel: f32,
    pub y_vel: f32,
    pub z_vel: f32,
    pub health: f32,
    pub shields: f32,
    pub camo: u8,
    pub shields_status: u16,
    pub melee_impact_this_tick: bool,
    pub primary_nades: u8,
    pub secondary_nades: u8,
    pub weapons: Vec<WeaponData>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct DamageEntry {
    pub damage_time: u32,
    pub damage_amount: f32,
    pub static_player_index: u32,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct DerivedStats {
    pub has_camo: bool,
    pub has_overshield: bool,
    pub shots_by_weapon: HashMap<String, u32>,
    pub damage_to_player: HashMap<u32, f32>,
    pub damage_from_player: HashMap<u32, f32>,
    pub damage_dealt: f32,
    pub damage_received: f32,
    pub camo_count: u32,
    pub overshield_count: u32,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct PlayerInfo {
    pub player_index: u32,
    pub local_player: i16,
    pub name: String,
    pub team: u32,
    pub respawn_timer: u32,
    pub camo_timer: u32,
    pub kill_streak: u16,
    pub multikill: u16,
    pub time_of_last_kill: i16,
    pub kills: i16,
    pub assists: i16,
    pub team_kills: i16,
    pub deaths: i16,
    pub suicides: i16,
    pub score: i32,
    pub ctf_score: i16,
    pub shots_fired: i32,
    pub shots_hit: i16,
    pub player_quit: u8,
    pub damage_table: Vec<DamageEntry>,
    pub observer_camera_info: Option<ObserverCamera>,
    pub player_object_data: Option<PlayerObjectData>,
    pub model_nodes: Vec<(f32, f32, f32)>,
    pub derived_stats: DerivedStats,
    pub input_data: Option<InputData>,
    
    pub machine_index: u8,
    pub controller_index: u8,
    pub color: u8,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct InputData {
    pub local_player_index: i32,
    pub buttons: InputButtons,
    pub sticks: InputSticks,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct InputButtons {
    pub a: u8,
    pub b: u8,
    pub x: u8,
    pub y: u8,
    pub black: u8,
    pub white: u8,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub start: u8,
    pub back: u8,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct InputSticks {
    pub left_vertical: f32,
    pub left_horizontal: f32,
    pub right_vertical: f32,
    pub right_horizontal: f32,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct NetworkGameClient {
    pub machine_index: u16,
    pub packets_sent: i16,
    pub packets_received: i16,
    pub average_ping: i16,
    pub seconds_to_game_start: i16,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ObserverCamera {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub x_aim: f32,
    pub y_aim: f32,
    pub z_aim: f32,
    pub fov: f32,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct GameGlobals {
    pub game_type: u32,
    pub variant: u8,
    pub has_teams: u8,
    pub map_name: String,
    pub can_score: bool,
    pub engine_running: bool,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct PlayerMeta {
    pub shots_by_weapon: HashMap<String, u32>,
    pub damage_to_player: HashMap<u32, f32>,
    pub damage_from_player: HashMap<u32, f32>,
    pub kills_by_player: HashMap<u32, u32>,
    pub deaths_by_player: HashMap<u32, u32>,
    pub shots_by_tick: HashMap<u32, u32>,
    pub kills_by_tick: HashMap<u32, u32>,
    pub deaths_by_tick: HashMap<u32, u32>,
    pub assists_by_tick: HashMap<u32, u32>,
    pub damage_dealt_by_tick: HashMap<u32, f32>,
    pub damage_dealt: f32,
    pub damage_received_by_tick: HashMap<u32, f32>,
    pub damage_received: f32,
    pub camo_by_tick: HashMap<u32, u32>,
    pub camo_count: u32,
    pub overshield_by_tick: HashMap<u32, u32>,
    pub overshield_count: u32,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct GameMeta {
    pub start_time: Option<String>,
    pub players: HashMap<u32, PlayerMeta>,
    pub events: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct GameTick {
    pub game_type: u32,
    pub variant: u8,
    pub game_engine_has_teams: bool,
    pub multiplayer_map_name: String,
    pub game_time_info: GameTimeInfo,
    pub damage_counts: HashMap<u32, HashMap<u32, f32>>,
    pub players: Vec<PlayerInfo>,
    pub objects: Vec<GameObject>,
    pub objects_meta: ObjectsMeta,
    pub spawns: Vec<Spawn>,
    pub items: Vec<Item>,
    pub fog: Option<FogInfo>,
    pub team_scores: Option<TeamScores>,
    pub network_client: Option<NetworkGameClient>,
    pub local_input: Option<InputData>,
    pub local_camera: Option<ObserverCamera>,
    pub game_engine_running: bool,
    pub game_engine_can_score: bool,
    pub current_time: String,
}

pub struct HaloState {
    pub reader: MemoryReader,
}

impl HaloState {
    pub fn new(reader: MemoryReader) -> Self {
        Self { reader }
    }

    pub async fn populate_memory_cache(&mut self) -> Result<()> {
        let game_state_base = self.reader.read_u32(GAME_STATE_BASE_PTR).await?;
        let game_state_size = self.reader.read_u32(GAME_STATE_SIZE_PTR).await?;
        let _ = self.reader.add_to_cache(game_state_base, game_state_size).await;

        let global_scenario = self.reader.read_u32(GLOBAL_SCENARIO_PTR).await?;
        let spawn_count = self.reader.read_s32(global_scenario + 852).await?;
        let first_spawn_addr = self.reader.read_s32(global_scenario + 856).await?;
        if first_spawn_addr != 0 && spawn_count > 0 {
             let _ = self.reader.add_to_cache(first_spawn_addr as u32, (52 * spawn_count) as u32).await;
        }

        let observer_camera_addr = 0x271550;
        let _ = self.reader.add_to_cache(observer_camera_addr, 688 * 4).await;

        let object_type_def_size = (0x1FCBA4 - 0x1FC0D0) * 2;
        let _ = self.reader.add_to_cache(OBJECT_TYPE_DEFINITIONS, object_type_def_size).await;

        Ok(())
    }

    pub fn invalidate_memory_cache(&mut self) {
        self.reader.invalidate_cache();
    }

    async fn get_object_type_string(&mut self, type_index: u8) -> String {
        let def_array = 0x1FCB78;
        let type_def_addr = self.reader.read_u32(def_array + 4 * (type_index as u32)).await.unwrap_or(0);
        if type_def_addr != 0 {
            let type_str_ptr = self.reader.read_u32(type_def_addr).await.unwrap_or(0);
             return self.reader.read_string(type_str_ptr, 64).await.unwrap_or_else(|_| "unknown".to_string());
        }
        "unknown".to_string()
    }

    pub async fn get_game_time_uncached(&mut self) -> Result<u32> {
        let addr = self.reader.read_u32_uncached(GAME_TIME_GLOBALS_PTR).await?;
        let game_time = self.reader.read_u32_uncached(addr + 12).await?;
        Ok(game_time)
    }

    pub async fn get_game_time_info(&mut self) -> Result<GameTimeInfo> {
        let addr = self.reader.read_u32(GAME_TIME_GLOBALS_PTR).await?;
        let game_time = self.reader.read_u32(addr + 12).await?.saturating_sub(1);
        let seconds = game_time / 30;
        let h = seconds / 3600;
        let m = (seconds % 3600) / 60;
        let s = seconds % 60;
        let real_time_elapsed = format!("{:02}:{:02}:{:02}", h, m, s);

        Ok(GameTimeInfo {
            game_time,
            game_time_elapsed: self.reader.read_u32(addr + 16).await?,
            real_time_elapsed,
            game_time_initialized: self.reader.read_u8(addr).await?,
            game_time_active: self.reader.read_u8(addr + 1).await?,
            game_time_paused: self.reader.read_u8(addr + 2).await?,
            game_time_speed: self.reader.read_f32(addr + 24).await?,
        })
    }

    pub async fn get_game_globals(&mut self) -> Result<GameGlobals> {
        let game_engine_addr = self.reader.read_u32(0x2F9110).await?;
        let engine_running = game_engine_addr != 0;
        let game_type = if engine_running { self.reader.read_u32(game_engine_addr + 0x4).await.unwrap_or(0) } else { 0 };
        let variant = self.reader.read_u8(0x2F90F4).await?;
        let has_teams = self.reader.read_u8(0x2F90C4).await?;
        let map_name = self.reader.read_string(0x2E37CD, 64).await.unwrap_or_default();
        let can_score_val = self.reader.read_u32(0x2FABF0).await?;
        let can_score = can_score_val == 0 && engine_running;

        Ok(GameGlobals { game_type, variant, has_teams, map_name, can_score, engine_running })
    }

    pub async fn get_spawns(&mut self) -> Result<Vec<Spawn>> {
        let global_scenario = self.reader.read_u32(GLOBAL_SCENARIO_PTR).await?;
        let spawn_count = self.reader.read_s32(global_scenario + 852).await?;
        let first_spawn_addr = self.reader.read_u32(global_scenario + 856).await?;
        let mut spawns = Vec::new();
        if spawn_count > 0 {
            for i in 0..spawn_count {
                let addr = first_spawn_addr + 52 * (i as u32);
                spawns.push(Spawn {
                    spawn_id: i as u32,
                    x: self.reader.read_f32(addr).await?,
                    y: self.reader.read_f32(addr + 4).await?,
                    z: self.reader.read_f32(addr + 8).await?,
                    facing: self.reader.read_f32(addr + 12).await?,
                    team_index: self.reader.read_u8(addr + 16).await?,
                    gametypes: vec![
                        self.reader.read_u8(addr + 20).await?,
                        self.reader.read_u8(addr + 21).await?,
                        self.reader.read_u8(addr + 22).await?,
                        self.reader.read_u8(addr + 23).await?,
                    ],
                });
            }
        }
        Ok(spawns)
    }

    pub async fn get_items(&mut self) -> Result<Vec<Item>> {
        let global_scenario = self.reader.read_u32(GLOBAL_SCENARIO_PTR).await?;
        let item_count = self.reader.read_s32(global_scenario + 900).await?;
        let first_item_addr = self.reader.read_u32(global_scenario + 904).await?;
        let global_tag_instances = self.reader.read_u32(GLOBAL_TAG_INSTANCES).await?;
        let mut items = Vec::new();
        if item_count > 0 {
            for i in 0..item_count {
                let addr = first_item_addr + 144 * (i as u32);
                let tag_index = self.reader.read_s32(addr + 0x5C).await?;
                if tag_index != -1 {
                    let tag_instance_addr = global_tag_instances + 32 * ((tag_index as u32) & 0xFFFF);
                    let name_ptr = self.reader.read_u32(tag_instance_addr + 0x10).await?;
                    let tag_name = self.reader.read_string(name_ptr, 128).await.unwrap_or_default();
                    items.push(Item {
                        tag_name,
                        x: self.reader.read_f32(addr + 0x40).await?,
                        y: self.reader.read_f32(addr + 0x44).await?,
                        z: self.reader.read_f32(addr + 0x48).await?,
                    });
                }
            }
        }
        Ok(items)
    }

    pub async fn get_fog(&mut self) -> Result<FogInfo> {
        Ok(FogInfo {
            color_r: self.reader.read_f32(FOG_PARAMS + 0x4).await?,
            color_g: self.reader.read_f32(FOG_PARAMS + 0x8).await?,
            color_b: self.reader.read_f32(FOG_PARAMS + 0xC).await?,
            max_density: self.reader.read_f32(FOG_PARAMS + 0x10).await?,
            atmo_min_dist: self.reader.read_f32(FOG_PARAMS + 0x14).await?,
            atmo_max_dist: self.reader.read_f32(FOG_PARAMS + 0x18).await?,
        })
    }

    pub async fn get_team_scores(&mut self) -> Result<TeamScores> {
        Ok(TeamScores {
            ctf_team_score: (self.reader.read_u32(TEAM_SCORES_CTF).await.unwrap_or(0), self.reader.read_u32(TEAM_SCORES_CTF + 4).await.unwrap_or(0)),
            ctf_score_limit: self.reader.read_u32(TEAM_SCORES_CTF + 8).await.unwrap_or(0),
            slayer_team_score: (self.reader.read_u32(TEAM_SCORES_SLAYER).await.unwrap_or(0), self.reader.read_u32(TEAM_SCORES_SLAYER + 4).await.unwrap_or(0)),
            slayer_score_limit: self.reader.read_u32(0x2F90E8).await.unwrap_or(0),
            oddball_team_score: (self.reader.read_u32(TEAM_SCORES_ODDBALL).await.unwrap_or(0), self.reader.read_u32(TEAM_SCORES_ODDBALL + 4).await.unwrap_or(0)),
            oddball_score_limit: self.reader.read_u32(TEAM_SCORES_ODDBALL - 4).await.unwrap_or(0),
            king_team_score: (self.reader.read_u32(TEAM_SCORES_KING).await.unwrap_or(0), self.reader.read_u32(TEAM_SCORES_KING + 4).await.unwrap_or(0)),
            race_team_score: (self.reader.read_u32(TEAM_SCORES_RACE).await.unwrap_or(0), self.reader.read_u32(TEAM_SCORES_RACE + 4).await.unwrap_or(0)),
        })
    }

    pub async fn get_network_client(&mut self) -> Result<NetworkGameClient> {
        Ok(NetworkGameClient {
            machine_index: self.reader.read_u16(NETWORK_GAME_CLIENT).await?,
            packets_sent: self.reader.read_s16(NETWORK_GAME_CLIENT + 2084).await?,
            packets_received: self.reader.read_s16(NETWORK_GAME_CLIENT + 2086).await?,
            average_ping: self.reader.read_s16(NETWORK_GAME_CLIENT + 2088).await?,
            seconds_to_game_start: self.reader.read_s16(NETWORK_GAME_CLIENT + 3236).await?,
        })
    }

    pub async fn get_observer_camera(&mut self, local_player_index: u32) -> Result<ObserverCamera> {
        let base_addr = 0x271550 + 167 * 4 * local_player_index;
        Ok(ObserverCamera {
            x: self.reader.read_f32(base_addr).await?,
            y: self.reader.read_f32(base_addr + 4).await?,
            z: self.reader.read_f32(base_addr + 8).await?,
            x_aim: self.reader.read_f32(base_addr + 32).await?,
            y_aim: self.reader.read_f32(base_addr + 36).await?,
            z_aim: self.reader.read_f32(base_addr + 40).await?,
            fov: self.reader.read_f32(base_addr + 56).await?,
        })
    }

    pub async fn get_input_data(&mut self, local_player_index: i32) -> Result<InputData> {
        let base_addr = INPUT_ABSTRACTION_GLOBALS + (0x1C * local_player_index as u32);
        Ok(InputData {
            local_player_index,
            buttons: InputButtons {
                a: self.reader.read_u8(base_addr).await?,
                black: self.reader.read_u8(base_addr + 1).await?,
                x: self.reader.read_u8(base_addr + 2).await?,
                y: self.reader.read_u8(base_addr + 3).await?,
                b: self.reader.read_u8(base_addr + 4).await?,
                white: self.reader.read_u8(base_addr + 5).await?,
                left_trigger: self.reader.read_u8(base_addr + 6).await?,
                right_trigger: self.reader.read_u8(base_addr + 7).await?,
                start: self.reader.read_u8(base_addr + 8).await?,
                back: self.reader.read_u8(base_addr + 9).await?,
            },
            sticks: InputSticks {
                left_vertical: self.reader.read_f32(base_addr + 0xC).await?,
                left_horizontal: self.reader.read_f32(base_addr + 0x10).await?,
                right_horizontal: self.reader.read_f32(base_addr + 0x14).await?,
                right_vertical: self.reader.read_f32(base_addr + 0x18).await?,
            }
        })
    }

    async fn get_tag_name(&mut self, addr: u32) -> String {
        let tag_idx = self.reader.read_s16(addr).await.unwrap_or(-1) as u32;
        if tag_idx != 0xFFFFFFFF {
            let inst_addr = self.reader.read_u32(GLOBAL_TAG_INSTANCES).await.unwrap_or(0);
            if inst_addr != 0 {
                let name_ptr = self.reader.read_u32(inst_addr + 32 * tag_idx + 0x10).await.unwrap_or(0);
                if name_ptr != 0 {
                    return self.reader.read_string(name_ptr, 128).await.unwrap_or_default();
                }
            }
        }
        String::new()
    }

    pub async fn get_objects(&mut self) -> Result<Vec<GameObject>> {
        let object_header = self.reader.read_u32(OBJECT_HEADER_DATUM_ARRAY).await?;
        let total_count = self.reader.read_u16(object_header + 0x2E).await?;
        let first_element = self.reader.read_u32(object_header + 0x34).await?;
        let mut objects = Vec::new();
        for i in 0..total_count {
            let res: Result<GameObject> = async {
                let object_addr = self.reader.read_u32(first_element + 12 * (i as u32) + 8).await?;
                if object_addr == 0 {
                    return Err(anyhow::anyhow!("null object"));
                }
                let object_type = self.reader.read_u8(object_addr + 0x64).await?;
                let tag_name = self.get_tag_name(object_addr).await;
                Ok(GameObject {
                    object_id: i as u32,
                    x: self.reader.read_f32(object_addr + 0xC).await?,
                    y: self.reader.read_f32(object_addr + 0x10).await?,
                    z: self.reader.read_f32(object_addr + 0x14).await?,
                    object_type_string: self.get_object_type_string(object_type).await,
                    tag_name: if !tag_name.is_empty() { Some(tag_name) } else { None },
                })
            }.await;

            if let Ok(obj) = res {
                objects.push(obj);
            }
        }
        Ok(objects)
    }

    pub async fn get_model_nodes(&mut self, base_addr: u32) -> Vec<(f32, f32, f32)> {
        let offsets = [
            0x4a8, 0x4dc, 0x510, 0x544, 0x578, 0x5ac, 0x5e0, 0x614, 0x648, 0x67c,
            0x6b0, 0x6e4, 0x718, 0x74c, 0x780, 0x7b4, 0x7e8, 0x81c, 0x850,
        ];
        let mut nodes = Vec::new();
        for offset in offsets {
            if let Ok(x) = self.reader.read_f32(base_addr + offset).await {
                if let Ok(y) = self.reader.read_f32(base_addr + offset + 4).await {
                    if let Ok(z) = self.reader.read_f32(base_addr + offset + 8).await {
                        nodes.push((x, y, z));
                    }
                }
            }
        }
        nodes
    }

    pub async fn get_players(&mut self, gametype: u32) -> Result<Vec<PlayerInfo>> {
        let player_datum_array = match self.reader.read_u32(PLAYER_DATUM_ARRAY).await {
            Ok(addr) => addr,
            Err(e) => return Err(anyhow::anyhow!("Failed to read player datum array: {}", e)),
        };
        let player_count = self.reader.read_u16(player_datum_array + 0x2E).await.unwrap_or(0);
        let first_element = self.reader.read_u32(player_datum_array + 0x34).await.unwrap_or(0);
        let element_size = self.reader.read_u16(player_datum_array + 0x22).await.unwrap_or(0) as u32;
        
        if first_element == 0 || element_size == 0 {
            return Ok(Vec::new());
        }

        let object_header = self.reader.read_u32(OBJECT_HEADER_DATUM_ARRAY).await.unwrap_or(0);
        let obj_first_element = if object_header != 0 { self.reader.read_u32(object_header + 0x34).await.unwrap_or(0) } else { 0 };
        let obj_element_size = if object_header != 0 { self.reader.read_u16(object_header + 0x22).await.unwrap_or(0) as u32 } else { 0 };
        
        let network_game_client_addr = NETWORK_GAME_CLIENT;
        let network_game_data_addr = network_game_client_addr + 2140;
        let network_players_addr = network_game_data_addr + 550;

        let mut players = Vec::new();
        for i in 0..player_count {
            let static_addr = first_element + (i as u32) * element_size;
            
            let player_res: Result<PlayerInfo> = async {
                let local_player = self.reader.read_s16(static_addr + 0x2).await?;
                let player_obj_handle = self.reader.read_s32(static_addr + 0x34).await?;
                let mut dynamic_player_addr = 0;
                let mut player_object_data = None;
                let mut damage_table = Vec::new();

                if player_obj_handle != -1 && obj_first_element != 0 {
                    dynamic_player_addr = self.reader.read_u32(obj_first_element + ((player_obj_handle as u32) & 0xFFFF) * obj_element_size + 8).await.unwrap_or(0);
                    if dynamic_player_addr != 0 {
                        let mut weapons = Vec::new();
                        for w in 0..4 {
                            let w_handle = self.reader.read_u32(dynamic_player_addr + 0x2A8 + 4 * w).await.unwrap_or(0xFFFFFFFF);
                            if w_handle != 0xFFFFFFFF {
                                let w_addr = self.reader.read_u32(obj_first_element + (w_handle & 0xFFFF) * obj_element_size + 8).await.unwrap_or(0);
                                if w_addr != 0 {
                                    let w_tag_idx = self.reader.read_s16(w_addr).await.unwrap_or(0) as u32;
                                    let w_tag_inst = self.reader.read_u32(GLOBAL_TAG_INSTANCES).await? + 32 * w_tag_idx;
                                    let w_name_ptr = self.reader.read_u32(w_tag_inst + 0x10).await?;
                                    let w_tag_def_addr = self.reader.read_u32(w_tag_inst + 0x20).await?;
                                    let w_type = self.reader.read_u8(w_tag_def_addr + 0x309).await.unwrap_or(0);
                                    weapons.push(WeaponData {
                                        tag_name: self.reader.read_string(w_name_ptr, 64).await.unwrap_or_default(),
                                        magazine_ammo_count: self.reader.read_s16(w_addr + 0x260).await.unwrap_or(0),
                                        charge_amount: self.reader.read_f32(w_addr + 0xF0).await.unwrap_or(0.0),
                                        is_energy_weapon: (w_type & 8) != 0,
                                        object_id: (w_handle & 0xFFFF) as u16,
                                    });
                                }
                            }
                        }
                        player_object_data = Some(PlayerObjectData {
                            x: self.reader.read_f32(dynamic_player_addr + 0xC).await?,
                            y: self.reader.read_f32(dynamic_player_addr + 0x10).await?,
                            z: self.reader.read_f32(dynamic_player_addr + 0x14).await?,
                            x_vel: self.reader.read_f32(dynamic_player_addr + 0x18).await?,
                            y_vel: self.reader.read_f32(dynamic_player_addr + 0x1C).await?,
                            z_vel: self.reader.read_f32(dynamic_player_addr + 0x20).await?,
                            health: self.reader.read_f32(dynamic_player_addr + 0x90).await?,
                            shields: self.reader.read_f32(dynamic_player_addr + 0x94).await?,
                            camo: self.reader.read_u8(dynamic_player_addr + 0x1B4).await?,
                            shields_status: self.reader.read_u16(dynamic_player_addr + 0xB6).await?,
                            melee_impact_this_tick: self.reader.read_u8(dynamic_player_addr + 0x45D).await.unwrap_or(0) == self.reader.read_u8(dynamic_player_addr + 0x45E).await.unwrap_or(1),
                            primary_nades: self.reader.read_u8(dynamic_player_addr + 0x2CE).await.unwrap_or(0),
                            secondary_nades: self.reader.read_u8(dynamic_player_addr + 0x2CF).await.unwrap_or(0),
                            weapons,
                        });
                        for d in 0..4 {
                            let d_base = dynamic_player_addr + 0x3E0 + (d * 16);
                            let time = self.reader.read_u32(d_base).await.unwrap_or(0xFFFFFFFF);
                            if time != 0xFFFFFFFF {
                                let attacker_handle = self.reader.read_u32(d_base + 12).await.unwrap_or(0xFFFFFFFF);
                                damage_table.push(DamageEntry { 
                                    damage_time: time, 
                                    damage_amount: self.reader.read_f32(d_base + 4).await.unwrap_or(0.0),
                                    static_player_index: attacker_handle & 0xFFFF,
                                });
                            }
                        }
                    }
                }
                
                // Network Data
                let net_player_addr = network_players_addr + (i as u32) * 32;
                let (machine_index, controller_index, color) = match self.reader.read_u8(net_player_addr + 28).await {
                    Ok(m) => (m, self.reader.read_u8(net_player_addr + 29).await.unwrap_or(0), self.reader.read_s16(net_player_addr + 24).await.unwrap_or(0) as u8),
                    Err(_) => (0, 0, 0)
                };

                let score = if gametype == 1 { 0 } else { 
                    let score_addr = match gametype { 2 => TEAM_SCORES_SLAYER, 3 => TEAM_SCORES_ODDBALL, 4 => TEAM_SCORES_KING, 5 => TEAM_SCORES_RACE, _ => 0 };
                    if score_addr != 0 {
                        self.reader.read_s32(score_addr + 64 + 4 * (i as u32)).await.unwrap_or(0)
                    } else {
                        0
                    }
                };

                Ok(PlayerInfo {
                    player_index: i as u32,
                    local_player,
                    name: self.reader.read_wchar(static_addr + 0x4, 12).await.unwrap_or_else(|_| format!("Player {}", i)),
                    team: self.reader.read_u32(static_addr + 0x20).await?,
                    respawn_timer: self.reader.read_u32(static_addr + 0x2C).await?,
                    camo_timer: self.reader.read_u32(static_addr + 0x68).await?,
                    kill_streak: self.reader.read_u16(static_addr + 0x92).await?,
                    multikill: self.reader.read_u16(static_addr + 0x94).await?,
                    time_of_last_kill: self.reader.read_s16(static_addr + 0x96).await?,
                    kills: self.reader.read_s16(static_addr + 0x98).await?,
                    assists: self.reader.read_s16(static_addr + 0xA0).await?,
                    team_kills: self.reader.read_s16(static_addr + 0xA8).await?,
                    deaths: self.reader.read_s16(static_addr + 0xAA).await?,
                    suicides: self.reader.read_s16(static_addr + 0xAC).await?,
                    shots_fired: self.reader.read_s32(static_addr + 0xAE).await?,
                    shots_hit: self.reader.read_s16(static_addr + 0xB2).await?,
                    score,
                    ctf_score: self.reader.read_s16(static_addr + 0xC4).await?,
                    player_quit: self.reader.read_u8(static_addr + 0xD1).await?,
                    damage_table,
                    observer_camera_info: if local_player != -1 { self.get_observer_camera(local_player as u32).await.ok() } else { None },
                    player_object_data: player_object_data.clone(),
                    model_nodes: if dynamic_player_addr != 0 { self.get_model_nodes(dynamic_player_addr).await } else { Vec::new() },
                    derived_stats: DerivedStats { 
                        has_camo: player_object_data.as_ref().map(|o| o.camo == 0x51).unwrap_or(false), 
                        has_overshield: player_object_data.as_ref().map(|o| o.shields_status == 0x10 || o.shields > 1.0).unwrap_or(false),
                        ..Default::default()
                    },
                    input_data: if local_player != -1 { self.get_input_data(local_player as i32).await.ok() } else { None },
                    machine_index,
                    controller_index,
                    color,
                })
            }.await;

            if let Ok(player) = player_res {
                players.push(player);
            }
        }
        Ok(players)
    }

    pub async fn get_game_info(&mut self) -> Result<GameTick> {
        let game_time_info = self.get_game_time_info().await?;
        let globals = self.get_game_globals().await?;
        
        let players = self.get_players(globals.game_type).await.unwrap_or_default();
        let objects = self.get_objects().await.unwrap_or_default();
        
        let mut damage_counts: HashMap<u32, HashMap<u32, f32>> = HashMap::new();
        for player in &players {
            let victim_idx = player.player_index;
            for dmg in &player.damage_table {
                 damage_counts.entry(dmg.static_player_index)
                     .or_default()
                     .insert(victim_idx, dmg.damage_amount);
            }
        }

        let mut object_indexes_by_type: HashMap<String, Vec<usize>> = HashMap::new();
        let mut object_ids_by_type: HashMap<String, Vec<u32>> = HashMap::new();
        for (idx, obj) in objects.iter().enumerate() {
            object_indexes_by_type.entry(obj.object_type_string.clone()).or_default().push(idx);
            object_ids_by_type.entry(obj.object_type_string.clone()).or_default().push(obj.object_id);
        }
        let objects_meta = ObjectsMeta { object_indexes_by_type, object_ids_by_type };

        let mut local_input = None;
        let mut local_camera = None;
        if game_time_info.game_time_active != 0 {
            local_input = self.get_input_data(0).await.ok();
            local_camera = self.get_observer_camera(0).await.ok();
        }

        Ok(GameTick {
            game_type: globals.game_type,
            variant: globals.variant,
            game_engine_has_teams: globals.has_teams != 0,
            multiplayer_map_name: globals.map_name,
            game_time_info,
            damage_counts,
            players,
            objects,
            objects_meta,
            spawns: self.get_spawns().await.unwrap_or_default(),
            items: self.get_items().await.unwrap_or_default(),
            fog: self.get_fog().await.ok(),
            team_scores: self.get_team_scores().await.ok(),
            network_client: self.get_network_client().await.ok(),
            local_input,
            local_camera,
            game_engine_running: globals.engine_running,
            game_engine_can_score: globals.can_score,
            current_time: Local::now().to_rfc3339(),
        })
    }
}