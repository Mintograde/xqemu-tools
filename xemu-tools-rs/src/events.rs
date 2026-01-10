use crate::halo::{PlayerMeta, GameTick};
use std::collections::HashMap;

pub fn extract_events(
    old: &Option<GameTick>, 
    new: &GameTick, 
    player_meta_map: &mut HashMap<u32, PlayerMeta>
) -> Vec<String> {
    let mut events = Vec::new();
    let game_time = new.game_time_info.game_time;

    let old = match old {
        Some(o) => o,
        None => return events,
    };

    if !old.game_engine_can_score && new.game_engine_can_score {
        events.push(format!("{}: New game started on {}", game_time, new.multiplayer_map_name));
    }

    if old.players.len() == new.players.len() {
        for (old_p, new_p) in old.players.iter().zip(new.players.iter()) {
            let meta = player_meta_map.entry(new_p.player_index).or_default();

            if new_p.kills > old_p.kills {
                let diff = (new_p.kills - old_p.kills) as u32;
                events.push(format!("{}: {} got a kill ({})", game_time, new_p.name, new_p.kills));
                *meta.kills_by_tick.entry(game_time).or_default() += diff;
            }
            if new_p.deaths > old_p.deaths {
                let diff = (new_p.deaths - old_p.deaths) as u32;
                events.push(format!("{}: {} died ({})", game_time, new_p.name, new_p.deaths));
                *meta.deaths_by_tick.entry(game_time).or_default() += diff;
            }
            if new_p.assists > old_p.assists {
                let diff = (new_p.assists - old_p.assists) as u32;
                events.push(format!("{}: {} got an assist ({})", game_time, new_p.name, new_p.assists));
                *meta.assists_by_tick.entry(game_time).or_default() += diff;
            }

            if new_p.derived_stats.has_camo && !old_p.derived_stats.has_camo {
                events.push(format!("{}: {} picked up camo", game_time, new_p.name));
                meta.camo_count += 1;
                *meta.camo_by_tick.entry(game_time).or_default() += 1;
            }
            if !new_p.derived_stats.has_camo && old_p.derived_stats.has_camo {
                events.push(format!("{}: {} lost camo", game_time, new_p.name));
            }
            if new_p.derived_stats.has_overshield && !old_p.derived_stats.has_overshield {
                events.push(format!("{}: {} picked up overshield", game_time, new_p.name));
                meta.overshield_count += 1;
                *meta.overshield_by_tick.entry(game_time).or_default() += 1;
            }
            if !new_p.derived_stats.has_overshield && old_p.derived_stats.has_overshield {
                events.push(format!("{}: {} lost overshield", game_time, new_p.name));
            }

            if let (Some(old_data), Some(new_data)) = (&old_p.player_object_data, &new_p.player_object_data) {
                if old_data.primary_nades > new_data.primary_nades {
                    events.push(format!("{}: {} threw frag grenade ({} -> {})", game_time, new_p.name, old_data.primary_nades, new_data.primary_nades));
                }
                if old_data.secondary_nades > new_data.secondary_nades {
                    events.push(format!("{}: {} threw plasma grenade ({} -> {})", game_time, new_p.name, old_data.secondary_nades, new_data.secondary_nades));
                }

                for (old_w, new_w) in old_data.weapons.iter().zip(new_data.weapons.iter()) {
                    if old_w.object_id == new_w.object_id {
                        let shot_diff = if new_w.is_energy_weapon {
                            if old_w.charge_amount > new_w.charge_amount { 1 } else { 0 }
                        } else {
                            if old_w.magazine_ammo_count > new_w.magazine_ammo_count {
                                (old_w.magazine_ammo_count - new_w.magazine_ammo_count) as u32
                            } else { 0 }
                        };
                        
                        if shot_diff > 0 {
                            *meta.shots_by_weapon.entry(new_w.tag_name.clone()).or_default() += shot_diff;
                            *meta.shots_by_tick.entry(game_time).or_default() += shot_diff;
                        }
                    }
                }
            }
        }
    }

    for new_p in &new.players {
        let old_p = old.players.iter().find(|p| p.player_index == new_p.player_index);
        let spawned = match old_p {
            Some(op) => op.player_object_data.is_none() && new_p.player_object_data.is_some(),
            None => new_p.player_object_data.is_some(),
        };

        if spawned {
            if let Some(obj_data) = &new_p.player_object_data {
                let mut spawn_found = false;
                for spawn in &new.spawns {
                    let dx = obj_data.x - spawn.x;
                    let dy = obj_data.y - spawn.y;
                    let dz = obj_data.z - spawn.z;
                    let dist = (dx*dx + dy*dy + dz*dz).sqrt();
                    
                    if dist <= 0.2 {
                        events.push(format!("{}: {} spawned at spawn id {}", game_time, new_p.name, spawn.spawn_id));
                        spawn_found = true;
                        break;
                    }
                }
                if !spawn_found {
                    events.push(format!("{}: {} spawned at an unknown spawn id ({:.2}, {:.2}, {:.2})", game_time, new_p.name, obj_data.x, obj_data.y, obj_data.z));
                }
            }
        }
    }

    for (&dealer_idx, victims) in &new.damage_counts {
        for (&victim_idx, &new_amount) in victims {
            let old_amount = old.damage_counts.get(&dealer_idx).and_then(|v| v.get(&victim_idx)).cloned().unwrap_or(0.0);
            if new_amount > old_amount {
                let diff = new_amount - old_amount;
                
                let dealer_name = new.players.iter().find(|p| p.player_index == dealer_idx).map(|p| p.name.as_str()).unwrap_or("Unknown");
                let victim_name = new.players.iter().find(|p| p.player_index == victim_idx).map(|p| p.name.as_str()).unwrap_or("Unknown");
                events.push(format!("{}: {} damaged {} for {:.2}", game_time, dealer_name, victim_name, diff));
                
                if let Some(dealer_meta) = player_meta_map.get_mut(&dealer_idx) {
                    dealer_meta.damage_dealt += diff;
                    *dealer_meta.damage_dealt_by_tick.entry(game_time).or_default() += diff;
                    *dealer_meta.damage_to_player.entry(victim_idx).or_default() += diff;
                } else {
                    let mut dealer_meta = crate::halo::PlayerMeta::default();
                    dealer_meta.damage_dealt += diff;
                    *dealer_meta.damage_dealt_by_tick.entry(game_time).or_default() += diff;
                    *dealer_meta.damage_to_player.entry(victim_idx).or_default() += diff;
                    player_meta_map.insert(dealer_idx, dealer_meta);
                }

                if let Some(victim_meta) = player_meta_map.get_mut(&victim_idx) {
                    victim_meta.damage_received += diff;
                    *victim_meta.damage_received_by_tick.entry(game_time).or_default() += diff;
                    *victim_meta.damage_from_player.entry(dealer_idx).or_default() += diff;
                } else {
                    let mut victim_meta = crate::halo::PlayerMeta::default();
                    victim_meta.damage_received += diff;
                    *victim_meta.damage_received_by_tick.entry(game_time).or_default() += diff;
                    *victim_meta.damage_from_player.entry(dealer_idx).or_default() += diff;
                    player_meta_map.insert(victim_idx, victim_meta);
                }
            }
        }
    }

    if old.game_engine_can_score && !new.game_engine_can_score {
        events.push(format!("{}: Game ended on {}", game_time, new.multiplayer_map_name));
    }

    events
}
