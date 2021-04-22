#!/bin/env python3
import queue
import threading
from collections import defaultdict
from dataclasses import dataclass
from pprint import pprint

from database import DBConnector
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

from pymem import Pymem
pm = Pymem('xemuw.exe')


db = DBConnector()


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

    last_request_time = datetime.datetime.now()
    request_rate_seconds = 0.005
    cmd_counter = 0
    cmd_counter_reset = datetime.datetime.now()

    def stop(self):
        if self._p:
            self._p.terminate()
            self._p = None

    def run_cmd(self, cmd):
        # print(f'running command: {cmd}')
        now = datetime.datetime.now()
        delta = (now - self.last_request_time).total_seconds()
        if delta < self.request_rate_seconds:
            # print(f'waiting {self.request_rate_seconds - delta}s')
            time.sleep(self.request_rate_seconds - delta)
        self.last_request_time = now
        if type(cmd) is str:
            cmd = {
                "execute": cmd,
                "arguments": {}
            }
        self.cmd_counter += 1
        if (datetime.datetime.now() - self.cmd_counter_reset).total_seconds() > 1.0:
            print(f'qmp commands in last {(datetime.datetime.now() - self.cmd_counter_reset).total_seconds()} seconds: {self.cmd_counter}')
            self.cmd_counter = 0
            self.cmd_counter_reset = datetime.datetime.now()
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
        r = response['return'].replace('\r', '')
        # print(f"response: {r}")
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

        # print('Getting host virtual address of guest physical address {}'.format(hex(addr)))

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
        # print(response)
        lines = response['return'].replace('\r', '').split('\n')
        data_string = ' '.join(l.partition(' is ')[2] for l in lines).strip()
        data = int(data_string, 16)
        return data

    def gpa2hpa(self, addr):
        cmd = {
            "execute": "human-monitor-command",
            "arguments": {"command-line": "gpa2hpa {}".format(addr)}
        }
        # print('Getting host physical address of guest physical address {}'.format(hex(addr)))
        response = self.run_cmd(cmd)
        # print(response)
        # lines = response['return'].replace('\r', '').split('\n')
        # data_string = ' '.join(l.partition('hpa: ')[2] for l in lines).strip()
        # data = int(data_string, 16)
        # return data

    def gva2gpa(self, addr):
        cmd = {
            "execute": "human-monitor-command",
            "arguments": {"command-line": "gva2gpa {}".format(addr)}
        }
        # print('Getting guest physical address of guest virtual address {}'.format(hex(addr)))
        response = self.run_cmd(cmd)
        # print(response)
        lines = response['return'].replace('\r', '').split('\n')
        data_string = ' '.join(l.partition('gpa: ')[2] for l in lines).strip()
        data = int(data_string, 16)
        return data

    def gva2hva(self, addr):
        return self.gpa2hva(self.gva2gpa(addr))


t = Test()


def connect():
    # The connection loop is JayFoxRox's code.
    i = 0
    while True:
        print('Trying to connect %d' % i)
        if i > 0: time.sleep(1)
        try:
            t._qmp = QEMUMonitorProtocol(('localhost', 4444))
            t._qmp.connect()
            t._qmp.settimeout(0.5)
        except Exception as e:
            if i > 4:
                raise
            else:
                i += 1
                continue
        break


connect()

"""
    read_u32(address, returns_address=False, is_host_address=False, watch=False)

    the first time a guest address is read, convert to host address using qmp
    read value using host address
    store guest address, host address, and value
    every time address is read (or just periodically? or on error? (but what's an error here?)), recheck value using host address
    if value is different, reconvert guest to host, reread value, and store new host+value

    if value was a guest address, also remove that guest address from the address cache

    would we only want to watch for changes if the value is an address?

    need to handle host addresses that are just offsets from other host addresses (is_host_address=True?)
    should those values also be stored in case they change?
    
    rate limit?
    
    also need to deal with reading the same address with multiple sizes
    
    the idea with offsets is to watch the struct header itself for changes, and trust that the rest of the struct remains at the same location
        (to prevent constantly checking on e.g. player weapon swaps or dynamic player address within static player)
        
    taking an example:
    
        players_globals_address = read_u32(0x2FAD20)
        
        the value returned is a guest virtual address
        


    read_host_u32(address, is_address=False, watch=False) 
"""


@dataclass
class Address:
    guest_address: int
    host_address: int = None
    returns_address: bool = False
    watch: bool = False


known_addresses = defaultdict(dict)


# TODO: implement automatic decoration with exclusion -- see http://stackoverflow.com/a/10067363
# def cache_addresses(f):
#     """If the decorated function fails due to a connection error, the decorator attempts
#     to reconnect the excepting DBConnection instance, then executes the decorated method.
#     :param f: the function being called
#     """
#
#     def wrapper(*args, **kwargs):
#         try:
#             return f(*args, **kwargs)
#         except pyodbc.Error:
#             pprint(sys.exc_info())
#             # if isinstance(args[0], DBConnection) and '08S01' in str(sys.exc_info()[1][1]):
#             if isinstance(args[0], DBConnection) and '08S01' in str(sys.exc_info()[1]):
#                 log.error('Database connection lost. Attempting to reconnect...')
#                 args[0].reconnect()
#                 return f(*args, **kwargs)
#             else:
#                 raise
#     return wrapper

new_tick = False
pymem_counter = 0


def read_memory(address, fn, retry_on_value_change=False, is_host_address=False, watch=False, **kwargs):
    """

    FIXME: I don't like the way I'm passing function handles into this function. Replace with something better.

    :param fn:
    :param address:
    :param retry_on_value_change:
    :param is_host_address:
    :param watch:
    :return:
    """

    global pymem_counter

    if is_host_address:
        value = fn(address, **kwargs)
        pymem_counter += 1

    elif address in known_addresses:

        # read value from host memory address if we've already translated the guest address
        value = fn(known_addresses[address]['host_address'], **kwargs)
        pymem_counter += 1

        # if value has changed and it should not have changed, translate address again and retry
        if retry_on_value_change and value != known_addresses[address]['value']:
            print(f'WARNING: value for {hex(address)} has changed from {hex(known_addresses[address]["value"])} to {hex(value)}')
            known_addresses[address]['host_address'] = t.gva2hva(address)

            # TODO: should we reread the value using the new host address?
            value = fn(known_addresses[address]['host_address'], **kwargs)
            pymem_counter += 1

        known_addresses[address]['value'] = value

    # translate guest address to host address if this is the first time we're seeing the guest address
    else:
        known_addresses[address]['host_address'] = t.gva2hva(address)
        value = fn(known_addresses[address]['host_address'], **kwargs)
        pymem_counter += 1
        known_addresses[address]['value'] = value
        # value = int.from_bytes(t.read(address, 4), 'little')

    # print(f'{hex(address)} -> {hex(watched_addresses[address]["host_address"]) if address in watched_addresses else ""}: {hex(value)}')
    return value


# These read functions were originally JayFoxRox's
def read_u8(address, *args, **kwargs):
    return read_memory(address, pm.read_uchar, *args, **kwargs)


def read_u16(address, *args, **kwargs):
    return read_memory(address, pm.read_ushort, *args, **kwargs)


def read_u32(address, *args, **kwargs):
    return read_memory(address, pm.read_uint, *args, **kwargs)


def read_s8(address, *args, **kwargs):
    return read_memory(address, pm.read_char, *args, **kwargs)


def read_s16(address, *args, **kwargs):
    return read_memory(address, pm.read_short, *args, **kwargs)


def read_s32(address, *args, **kwargs):
    return read_memory(address, pm.read_int, *args, **kwargs)


def read_float(address, *args, **kwargs):
    return read_memory(address, pm.read_float, *args, **kwargs)


def read_bytes(address, length, *args, **kwargs):
    return read_memory(address, pm.read_bytes, length=length, *args, **kwargs)


player_datum_array = read_u32(0x2FAD28)
player_datum_array_max_count = read_u16(player_datum_array + Datum_Array_Element_Max_Count_Offset)
player_datum_array_element_size = read_u16(player_datum_array + Datum_Array_Element_Size_Offset)
player_datum_array_first_element_address = read_u32(player_datum_array + Datum_Array_First_Element_Pointer_Offset)

t.gpa2hva(0x2FAD20)
gpa = t.gva2gpa(player_datum_array_first_element_address)
hva = t.gpa2hva(gpa)
hpa = t.gpa2hpa(gpa)

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
game_globals_276 = read_u32(game_globals_address + 276)
game_globals_276_108 = read_u16(game_globals_address + 108)

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


def get_game_info():

    player_count = read_u16(player_datum_array + 0x2E)
    player_stat_array = []

    game_time = read_u32(game_time_globals_address + 12)
    game_time_elapsed = read_u32(game_time_globals_address + 16)
    # print(game_time, game_time_elapsed)

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
                dynamic_player_address=dynamic_player_address,
                dynamic_player_address_hex=hex(dynamic_player_address),
                player_object_id=player_object_id,
                object_header_datum_array_max_elements=object_header_datum_array_max_elements,
                object_header_datum_array_element_size=object_header_datum_array_element_size,
            )

            if player_object_handle != -1:

                # gpa = t.gva2gpa(dynamic_player_address)
                # hva = t.gpa2hva(gpa)
                # print(hex(hva))

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
                    flashlight=read_u8(dynamic_player_address + 0x1B6),
                    current_action=read_u32(dynamic_player_address + 0x1B8),    # multi bitfield: some functions only check second byte
                                                                                # 0x0000=no_action
                                                                                # 0x0001=crouch
                                                                                # 0x0002=jump
                                                                                # 0x0008=fire
                                                                                # 0x0010=flashlight    immediately goes back to 0x0 even if held
                                                                                # 0x0440=press_action    cycles back to 0x0 before going to 0x4000
                                                                                # 0x4000=hold_action
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
                # print(player_object_data['current_action'])

            else:

                player_object_data = {}
                print('player respawns in {} ticks'.format(read_u32(static_player_address + 0x2C)))

            # print(player_object_data['xaim2'], player_object_data['yaim2'], player_object_data['zaim2'])

            # TODO: game_engine_get_state_message()
            # TODO: game_statistics_record_kill() player_data+0xA0

            player_stats = dict(
                local_player=read_u16(static_player_address + 0x2),
                name=read_bytes(static_player_address + 0x4, 24).decode('utf-16').split('\x00', 1)[0],
                # name=t.read(static_player_address + 0x4, 24).decode('utf-16').split('\x00', 1)[0],

                team=read_u32(static_player_address + 0x20),
                action=read_u16(static_player_address + 0x28),  # 6 if standing over weapon (7 if only 1 weapon held), 8 if next to vehicle, 0 otherwise
                respawn_timer=read_u32(static_player_address + 0x2C),
                respawn_penalty=read_u32(static_player_address + 0x30),
                object_ref=read_u32(static_player_address + 0x34),  # -1 when player is dead
                object_index=read_u16(static_player_address + 0x34),
                object_id=read_u16(static_player_address + 0x36),
                copy_of_object_index_id=read_u32(static_player_address + 0x38),  #  0x34 gets copied here when player dies
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

            # print(player_stats['action'])

            # players_globals = dict(
            #     local_player_count=read_u16(players_globals_address + 0x24)
            # )

            # pprint(player_stats)

            player_stat_array.append(player_stats)

    game_info = dict(
        game_time=game_time,
        game_time_elapsed=game_time_elapsed,
        game_globals_map_loaded=game_globals_map_loaded,
        game_globals_active=game_globals_active,
        game_time_initialized=game_time_initialized,
        game_time_active=game_time_active,
        game_time_paused=game_time_paused,
        game_time_speed=game_time_speed,
        game_time_leftover_dt=game_time_leftover_dt,
        main_menu_is_active=main_menu_is_active,
        last_game_in_progress=last_game_in_progress,
        last_game_connection=last_game_connection,
        players=player_stat_array,
        current_time=datetime.datetime.now(),
    )

    return game_info


def get_game_data():

    team_game_address = 0x2F90C4
    game_engine_address = 0x2F9110
    game_server_address = 0x2E3628
    game_client_address = 0x2E362C
    game_connection_word = 0x2E3684
    players_globals_address = 0x2FAD20
    team_data_address = 0x2FAD24


    # players_globals_is_dead


def send_to_file(data, outfile):

    # TODO: better serialization of datetime
    json.dump(data, outfile, default=str)


def send_to_database(game_info):

    for player in game_info['players']:
        if dynamic := player['player_object_data']:
            location = (dynamic['x'], dynamic['y'], dynamic['z'])
        else:
            location = None
        data = dict(
            time=game_info['current_time'],
            player=player['local_player'],
            tick=game_info['game_time'],
            location=location
        )
        db.insert_player_data(data)


game_info_queue = queue.Queue()


def handle_game_info_loop():

    with open("jsdklfjl.json", "a") as outfile:

        while True:

            game_info = game_info_queue.get()
            send_to_database(game_info)
            # print('inserted into database')
            # send_to_file(game_info, outfile)


database_worker_thread = threading.Thread(target=handle_game_info_loop, daemon=True)
database_worker_thread.start()


default_framerate_address = 0xBB648
refresh_rate_address = 0x1F8C98

game_time_address = game_time_globals_address + 12
gpa = t.gva2gpa(game_time_address)
game_time_hva = t.gpa2hva(gpa)
print(f'game_time: {hex(game_time_address)} -> {hex(game_time_hva)}')
hva = 0x208081c4624
print(hva, hex(hva))

gpa = t.gva2gpa(game_globals_276)
global_game_globals_276_hva = t.gpa2hva(gpa)
print(hex(game_globals_276_108), game_globals_276_108)  # 0???

game_info = get_game_info()
for i, player in enumerate(game_info['players']):
    if player['player_object_debug']['dynamic_player_address']:
        addr = player['player_object_debug']['dynamic_player_address']
        print(f"dynamic_player_address {hex(addr)} -> {hex(t.gpa2hva(t.gva2gpa(addr)))}")
        if i == 0:
            hva = t.gpa2hva(t.gva2gpa(addr))

counter = 0
last = 0
last_game_time = 0


# timing tests:
#  with 2 players:
#   no i/o              30 measurements/sec
#   to_file             20 measurements/sec
#   database             9 measurements/sec
#   db thread           26 measurements/sec
#   db+file thread      22 measurements/sec
#   put in queue only   28 measurements/sec

previous_game_info = None

with open("jkflsdjfkl.json", "a") as outfile:

    while True:

        try:

            action = pm.read_float(hva + 0xC)  # dynamic player x pos
            # action = pm.read_int(hva + 440)  # dynamic player + 440
            game_time = read_u32(game_time_address)
            # game_time = pm.read_uint(game_time_hva)

            game_info = get_game_info()

            # print(game_time, counter)
            counter += 1
            if game_time != last_game_time:
                print(f'{datetime.datetime.now()} new tick: {game_time} with {counter} measurements and {pymem_counter} pymem reads last tick')
                # print(f'{game_info_queue.qsize()} items in queue')
                counter = 0
                pymem_counter = 0

                # we only want to store the latest-updated game_info from the previous tick
                # if previous_game_info:
                #     game_info_queue.put(previous_game_info)

                # if game_info['players'][1]['player_object_data']:
                #     health = game_info['players'][1]['player_object_data']['health']
                #     shields = game_info['players'][1]['player_object_data']['shields']
                #     print(f'{health=} {shields=}')
            if action != last:
                print(f'{datetime.datetime.now()} new action: {game_time} {action} after {counter} measurements')
                pass
            last_game_time = game_time
            last = action

            # send_to_file(game_info, outfile)

            # send_to_database(game_info)

            # game_info_queue.put(game_info)

            # previous_game_info = game_info

            pass
            # pprint(players)
            # print(f'{datetime.datetime.now()} '
            #       f'health: {players[0]["player_object_data"]["health"]} '
            #       f'shields: {players[0]["player_object_data"]["shields"]} '
            #       f'max_health: {players[0]["player_object_data"]["max_health"]} '
            #       f'max_shields: {players[0]["player_object_data"]["max_shields"]}')
            # items = get_items()
            # print('===========')
            # pprint(items)
            # print()
            # print(game_info)

        except ValueError as e:

            # pass
            pprint(e)
            # raise

        except KeyError as e:

            pprint(e)

        except socket.timeout as e:

            print('DROPPED FRAME DUE TO SOCKET TIMEOUT')
            t._qmp.close()
            connect()
            # t._qmp.connect()

        else:

            for client in clients:

                pass
                client.sendMessage(json.dumps(game_info))

        # time.sleep(1)
        # time.sleep(0.033)

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
