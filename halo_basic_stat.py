#!/bin/env python3
import threading
from pprint import pprint

from qmp import QEMUMonitorProtocol
import sys
import os, os.path
import json
import subprocess
import time
import socket
import struct
import datetime
from memory_mappings_and_offsets import *

from SimpleWebSocketServer import SimpleWebSocketServer, WebSocket


clients = []
server = None


class SimpleWSServer(WebSocket):
    def handleConnected(self):
        print('Client connected', self)
        clients.append(self)

    def handleClose(self):
        print('Client disconnected', self)
        clients.remove(self)


def run_server():
    global server
    server = SimpleWebSocketServer('', 9000, SimpleWSServer,
                                   selectInterval=(1000.0 / 60) / 1000)
    print('Websocket server started', server)
    server.serveforever()


t = threading.Thread(target=run_server)
t.start()


# The test class is JayFoxRox's code.
class Test(object):

    def stop(self):
        if self._p:
            self._p.terminate()
            self._p = None

    def run_cmd(self, cmd):
        if type(cmd) is str:
            cmd = {
                "execute": cmd,
                "arguments": {}
            }
        resp = self._qmp.cmd_obj(cmd)
        if resp is None:
            raise Exception('Disconnected!')
        return resp

    def pause(self):
        return self.run_cmd('stop')

    def cont(self):
        return self.run_cmd('cont')

    def restart(self):
        return self.run_cmd('system_reset')

    def screenshot(self):
        cmd = {
            "execute": "screendump",
            "arguments": {
                "filename": "screenshot.ppm"
            }
        }
        return self.run_cmd(cmd)

    def isPaused(self):
        resp = self.run_cmd('query-status')
        return resp['return']['status'] == 'paused'

    def read(self, addr, size):
        cmd = {
            "execute": "human-monitor-command",
            "arguments": {"command-line": "x /%dxb %d" % (size, addr)}
        }
        response = self.run_cmd(cmd)
        lines = response['return'].replace('\r', '').split('\n')
        data_string = ' '.join(l.partition(': ')[2] for l in lines).strip()
        data = bytes(int(b, 16) for b in data_string.split(' '))
        return data

        # 'Cannot access memory'

    def gpa2hva(self, addr):
        cmd = {
            "execute": "human-monitor-command",
            "arguments": {"command-line": "gpa2hva {}".format(addr)}
        }

        print('Getting virtual address of {}'.format(addr))

        # Example responses:
        #   > gpa2hva 0x2fad20
        #   Host virtual address for 0x2fad20 (xbox.ram) is 000000001262ad20
        #
        #   > gpa2hva 0x4000023023
        #   No memory is mapped at address 0x4000023023
        #
        #   > gpa2hva jkflsdf
        #   invalid char 'j' in expression
        #   Try "help gpa2hva" for more information
        response = self.run_cmd(cmd)
        print(response)


t = Test()
i = 0

# The connection loop is JayFoxRox's code.
while True:
    print('Trying to connect %d' % i)
    if i > 0: time.sleep(1)
    try:
        t._qmp = QEMUMonitorProtocol(('localhost', 4444))
        t._qmp.connect()
    except Exception as e:
        if i > 4:
            raise
        else:
            i += 1
            continue
    break


# These read functions were originally JayFoxRox's
def read_u8(address):
    return int.from_bytes(t.read(address, 1), 'little')


def read_u16(address):
    return int.from_bytes(t.read(address, 2), 'little')


def read_u32(address):
    return int.from_bytes(t.read(address, 4), 'little')


def read_s8(address):
    return int.from_bytes(t.read(address, 1), 'little', signed=True)


def read_s16(address):
    return int.from_bytes(t.read(address, 2), 'little', signed=True)


def read_s32(address):
    return int.from_bytes(t.read(address, 4), 'little', signed=True)


def read_float(address):
    return struct.unpack("<f", t.read(address, 4))[0]


player_datum_array = read_u32(0x2FAD28)
player_datum_array_max_count = read_u16(player_datum_array + Datum_Array_Element_Max_Count_Offset)
player_datum_array_element_size = read_u16(player_datum_array + Datum_Array_Element_Size_Offset)
player_datum_array_first_element_address = read_u32(player_datum_array + Datum_Array_First_Element_Pointer_Offset)

t.gpa2hva(0x2FAD20)
t.gpa2hva(player_datum_array)
t.gpa2hva('sdfsdfsdf')

pprint(dict(
    player_datum_array=hex(player_datum_array),
    player_datum_array_max_count=player_datum_array_max_count,
    player_datum_array_element_size=player_datum_array_element_size,
    player_datum_array_first_element_address=hex(player_datum_array_first_element_address)
))

players_globals_address = read_u32(0x2FAD20)
teams_address = read_u32(0x2FAD24)
game_globals_address = read_u32(0x39BE4C)
game_engine_globals_address = read_u32(0x2F9110)
game_server_address = read_u32(0x2E3628)
game_client_address = read_u32(0x2E362C)
# game_connection_word = read_u16(0x2E3684)
game_connection_address = 0x2E3684
is_team_game_address = read_u8(0x2F90C4)
game_time_globals_address = read_u32(0x2F8CA0)
global_scenario_address = read_u32(0x39BE5C)
global_tag_instances_address = read_u32(0x39CE24)

# network game server
# total_players = read_u16(game_server_address + 0x224)
# max_players = read_u16(game_server_address + 0x10E)
# print('total players: {}'.format(total_players))
# print('max players: {}'.format(max_players))

something_saying_main_menu = read_u32(0x2E4000 + 4)


def get_items():

    # from game_engine_update_item_spawn()
    item_count = read_s32(global_scenario_address + 900)
    first_item_address = read_u32(global_scenario_address + 904)
    items = []
    if item_count > 0:
        for item_index in range(item_count):
            item_address = first_item_address + 144 * item_index
            item_spawn_interval = read_s16(item_address + 0xE)
            if not item_spawn_interval:
                tag_address = read_s32(item_address + 0x5C)
                tag_address_u = read_u32(item_address + 0x5C)
                if tag_address != -1:
                    # print(hex(tag_address), '|', tag_address, '|', hex(tag_address_u), '|', tag_address_u)
                    # print(hex(tag_address_u & 0xFFFF))
                    # print(32 * (tag_address & 0xFFFF) + global_tag_instances_address + 0x14)
                    item_spawn_interval = read_s16(read_s32(global_tag_instances_address + 32 * (tag_address & 0xFFFF) + 0x14) + 0xC)
                    print(item_spawn_interval, '---', t.read(read_s32(global_tag_instances_address + 32 * (tag_address & 0xFFFF) + 0x14), 32).decode("utf-8", 'ignore'))
                    print(t.read(global_tag_instances_address + 32 * (tag_address & 0xFFFF), 32))#.decode("utf-8", 'ignore'))
            item = dict(
                item_spawn_interval=item_spawn_interval,
                item_x=read_float(item_address + 0x40),
                item_y=read_float(item_address + 0x44),
                item_z=read_float(item_address + 0x48)
            )
            items.append(item)
    return items


last_game_connection = ''
last_game_in_progress = (0,0,0)


def get_players():

    player_count = 2
    player_stat_array = []

    game_time = read_u32(game_time_globals_address + 12)
    game_time_elapsed = read_u32(game_time_globals_address + 16)
    # print(game_time)

    game_globals_map_loaded = read_u8(game_globals_address)
    game_globals_active = read_u8(game_globals_address + 1)

    game_time_initialized = read_u8(game_time_globals_address)
    game_time_active = read_u8(game_time_globals_address + 1)
    game_time_paused = read_u8(game_time_globals_address + 2)
    game_time_speed = read_float(game_time_globals_address + 24)  # 1.0 is normal speed
    game_time_leftover_dt = read_float(game_time_globals_address + 28)

    main_menu_is_active = read_u8(0x2E4068)

    # game_in_progress
    #   splitscreen
    #   1 1 0 = ingame/postgame/mainmenu
    #   1 0 1 = choose map / pregame / singleplayer paused
    #   1 0 0 = briefly while loading game or changing from postgame to choose map screen
    #   0 0 0 = briefly after singleplayer save and quit (between 110 ingame and 110 main menu)
    global last_game_in_progress,last_game_connection
    if last_game_in_progress != (game_time_initialized, game_time_active, game_time_paused):
        print('game in progress changed to {} {} {}'.format(game_time_initialized, game_time_active, game_time_paused))
        last_game_in_progress = (game_time_initialized, game_time_active, game_time_paused)

    # game_connection
    #   0 = menus or singleplayer
    #   1 = system link -- looking for games / joined in network pregame
    #   2 = splitscreen -- hosting pregame lobby waiting for players
    #       system link -- hosting pregame (starts when pressing A on 'looking for games' screen)
    #   3 = watching 'saved film'
    game_connection = read_u16(game_connection_address)
    if last_game_connection != game_connection:
        print('game_connection changed to {}'.format(hex(game_connection)))
        last_game_connection = game_connection

    if game_time_initialized and game_time_active and not main_menu_is_active:

        for player_index in range(player_count):
            static_player_address = player_datum_array_first_element_address + player_index * player_datum_array_element_size
            player_object_handle = read_s32(static_player_address + 0x34)
            object_header_datum_array = read_u32(0x2FC6AC)
            object_header_datum_array_max_elements = read_u16(object_header_datum_array + 0x20)
            object_header_datum_array_element_size = read_u16(object_header_datum_array + 0x22)
            object_header_datum_array_first_element_address = read_u32(object_header_datum_array + 0x34)
            player_object_id = player_object_handle & 0xFFFF
            dynamic_player_address = read_u32(object_header_datum_array_first_element_address + (
                        player_object_handle & 0xFFFF) * object_header_datum_array_element_size + 8)

            # print('dynamic player address: {} | {}'.format(hex(dynamic_player_address), dynamic_player_address))
            # print('player_object_handle: {} | {}'.format(hex(player_object_handle), player_object_handle))

            player_object_debug = dict(
                player_object_handle=hex(player_object_handle),
                object_header_datum_array=hex(object_header_datum_array),
                object_header_datum_array_first_element_address=hex(object_header_datum_array_first_element_address),
                dynamic_player_address=hex(dynamic_player_address),
                player_object_id=player_object_id,
                object_header_datum_array_max_elements=object_header_datum_array_max_elements,
                object_header_datum_array_element_size=object_header_datum_array_element_size,
            )

            if player_object_handle != -1:

                player_object_data = dict(
                    x=read_float(dynamic_player_address + 0xC),
                    y=read_float(dynamic_player_address + 0x10),
                    z=read_float(dynamic_player_address + 0x14),
                    pitch=read_float(dynamic_player_address + 0x24),  # legs?
                    yaw=read_float(dynamic_player_address + 0x28),  # legs?
                    roll=read_float(dynamic_player_address + 0x2C),  # legs?
                    head_x=read_float(dynamic_player_address + 0x50),
                    head_y=read_float(dynamic_player_address + 0x54),
                    head_z=read_float(dynamic_player_address + 0x58),
                    max_health=read_float(dynamic_player_address + 0x88),
                    max_shields=read_float(dynamic_player_address + 0x8C),
                    health=read_float(dynamic_player_address + 0x90),
                    shields=read_float(dynamic_player_address + 0x94),
                    camo=read_u8(dynamic_player_address + 0x1B4),
                    xaim0=read_float(dynamic_player_address + 0x1F8),
                    yaim0=read_float(dynamic_player_address + 0x1FC),
                    zaim0=read_float(dynamic_player_address + 0x200),
                    xaim1=read_float(dynamic_player_address + 0x204),
                    yaim1=read_float(dynamic_player_address + 0x208),
                    zaim1=read_float(dynamic_player_address + 0x20C),
                    xaim2=read_float(dynamic_player_address + 0x210),
                    yaim2=read_float(dynamic_player_address + 0x214),
                    zaim2=read_float(dynamic_player_address + 0x218),
                    primary_nades=read_u8(dynamic_player_address + 0x2CE),
                    secondary_nades=read_u8(dynamic_player_address + 0x2CF),
                    crouchscale=read_float(dynamic_player_address + 0x464),
                )

            else:

                player_object_data = {}
                print('player respawns in {} ticks'.format(read_u32(static_player_address + 0x2C)))

            # print(player_object_data['xaim2'], player_object_data['yaim2'], player_object_data['zaim2'])

            # TODO: game_engine_get_state_message()
            # TODO: game_statistics_record_kill() player_data+0xA0

            player_stats = dict(
                local_player=read_u16(static_player_address + 0x2),
                name=t.read(static_player_address + 0x4, 24).decode('utf-16').split('\x00', 1)[0],
                team=read_u32(static_player_address + 0x20),
                respawn_timer=read_u32(static_player_address + 0x2C),
                respawn_penalty=read_u32(static_player_address + 0x30),
                object_ref=read_u32(static_player_address + 0x34),  # -1 when player is dead
                object_index=read_u16(static_player_address + 0x34),
                object_id=read_u16(static_player_address + 0x36),
                # copy_of_object_index_id=read_u32(static_player_address + 0x38),  #  0x34 gets copied here when player dies
                player_speed=read_float(static_player_address + 0x6C),
                camo_timer=read_u32(static_player_address + 0x68),
                target_player_index=read_u32(static_player_address + 0x88),
                kill_streak=read_u16(static_player_address + 0x92),  # resets to 0 on death
                multikill=read_u16(static_player_address + 0x94),  # resets to 0 on death
                time_of_last_kill=read_u16(static_player_address + 0x96),  # in ticks, resets to -1 on death
                kills=read_s16(static_player_address + 0x98),
                # assists=read_s16(static_player_address + 0xA0),  # not sure if assists
                team_kills=read_s16(static_player_address + 0xA8),
                deaths=read_s16(static_player_address + 0xAA),
                suicides=read_s16(static_player_address + 0xAC),
                player_quit=read_u8(static_player_address + 0xD1),  # 1 if player quit, not sure what else
                player_object_debug=player_object_debug,
                player_object_data=player_object_data,
            )

            # players_globals = dict(
            #     local_player_count=read_u16(players_globals_address + 0x24)
            # )

            # pprint(player_stats)

            player_stat_array.append(player_stats)

    return player_stat_array


def get_game_data():

    team_game_address = 0x2F90C4
    game_engine_address = 0x2F9110
    game_server_address = 0x2E3628
    game_client_address = 0x2E362C
    game_connection_word = 0x2E3684
    players_globals_address = 0x2FAD20
    team_data_address = 0x2FAD24


    # players_globals_is_dead


while True:

    try:

        # players = {}
        players = get_players()
        # items = get_items()
        # print('===========')
        # pprint(items)
        # print()

    except ValueError as e:

        pprint(e)
        raise

    else:

        for client in clients:

            client.sendMessage(json.dumps({'players': players}))

        time.sleep(0.033)

# while True:
#     print(dict(
#         x=read_float(object_data_address + 0xC),
#         y=read_float(object_data_address + 0x10),
#         z=read_float(object_data_address + 0x14),
#         pitch=read_float(object_data_address + 0x24),
#         yaw=read_float(object_data_address + 0x28),
#     ))
#     time.sleep(0.01)

# while True:
#
#     score1_new = read_s8(Team_Slayer_Red_Score_Address)  # 0x276710 is location of red team score in team slayer games
#     score2_new = read_s8(Team_Slayer_Blue_Score_Address)  # 0x276714 is location of blue team score in team slayer games
#
#     # if score changes
#     if (score1 != score1_new or score2 != score2_new):
#         # print it
#         print("Red = " + str(score1_new) + ", Blue = " + str(score2_new))
#         score1 = score1_new
#         score2 = score2_new
#
#         # identify what stats changed
#         player_stat_array_new = []
#         for player_index in range(player_count):
#             player_stats = {}
#             player_stats['kills'] = read_s8(
#                 player_datum_array_first_element_address + player_index * player_datum_array_element_size + Kills_Offset)
#             player_stats['team_kills'] = read_s8(
#                 player_datum_array_first_element_address + player_index * player_datum_array_element_size + Team_Kills_Offset)
#             player_stats['deaths'] = read_s8(
#                 player_datum_array_first_element_address + player_index * player_datum_array_element_size + Deaths_Offset)
#             player_stats['suicides'] = read_s8(
#                 player_datum_array_first_element_address + player_index * player_datum_array_element_size + Suicides_Offset)
#             player_stats['name'] = t.read(
#                 player_datum_array_first_element_address + player_index * player_datum_array_element_size + Player_Name_Offset,
#                 24).decode("utf-16").split('\x00', 1)[0]
#             player_stats['index'] = player_index
#             player_stat_array_new.append(player_stats)
#
#         # output stat changes
#         for player_index in range(player_count):
#             if player_stat_array[player_index]['kills'] != player_stat_array_new[player_index]['kills']:
#                 print(str(player_stat_array[player_index]['name']) + " kills changed from " + str(
#                     player_stat_array[player_index]['kills']) + " to " + str(
#                     player_stat_array_new[player_index]['kills']))
#             if player_stat_array[player_index]['deaths'] != player_stat_array_new[player_index]['deaths']:
#                 print(str(player_stat_array[player_index]['name']) + " deaths changed from " + str(
#                     player_stat_array[player_index]['deaths']) + " to " + str(
#                     player_stat_array_new[player_index]['deaths']))
#             if player_stat_array[player_index]['team_kills'] != player_stat_array_new[player_index]['team_kills']:
#                 print(str(player_stat_array[player_index]['name']) + " team_kills changed from " + str(
#                     player_stat_array[player_index]['team_kills']) + " to " + str(
#                     player_stat_array_new[player_index]['team_kills']))
#             if player_stat_array[player_index]['suicides'] != player_stat_array_new[player_index]['suicides']:
#                 print(str(player_stat_array[player_index]['name']) + " suicides changed from " + str(
#                     player_stat_array[player_index]['suicides']) + " to " + str(
#                     player_stat_array_new[player_index]['suicides']))
#         player_stat_array = player_stat_array_new
#     time.sleep(0.1)  # wait 100ms
