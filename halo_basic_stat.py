#!/bin/env python3
import copy
import gzip
import json
import lzma
import math
import queue
import re
import threading
from collections import defaultdict
from dataclasses import dataclass
from pprint import pprint, pformat

import brotli
import psutil

from database import DBConnector
from qmp import QEMUMonitorProtocol
import sys
import os, os.path
import orjson
import subprocess
import time
import socket
import struct
import datetime
# from memory_mappings_and_offsets import *

from SimpleWebSocketServer import SimpleWebSocketServer, WebSocket

from pymem import Pymem


def get_pid():
    """Returns the pid of the first xemu instance that has qmp running"""
    instances = []
    for proc in psutil.process_iter():
        if proc.name() == 'xemu.exe':
            info = proc.as_dict()
            cmdline = ' '.join(info['cmdline'])
            match = re.search(r'-qmp tcp:(?P<address>.+):(?P<port>\d+),', cmdline)
            if match:
                # instances.append(XemuInstance(pid=proc.pid,
                #                               qmp_address=match.group('address'),
                #                               qmp_port=match.group('port')))
                return proc.pid
            else:
                continue


pid = get_pid()
print(f'xemu pid is {pid} ({hex(pid)})')
# pm = Pymem('xemu.exe')
pm = Pymem()
pm.open_process_from_id(process_id=pid)

clients = []
server = None


class SimpleWSServer(WebSocket):
    def handleConnected(self):
        print('Websocket client connected', self)
        clients.append(self)

    def handleClose(self):
        print('Websocket client disconnected', self)
        clients.remove(self)


def run_server():
    global server
    server = SimpleWebSocketServer('', 9000, SimpleWSServer,
                                   selectInterval=(1000.0 / 60) / 1000)
    print('Websocket server started', server)
    server.serveforever()


server_thread = threading.Thread(target=run_server, daemon=True, name='websocket_server_thread')
server_thread.start()


# The test class is JayFoxRox's code.
class Test(object):

    last_request_time = datetime.datetime.now()
    rate_limit_enabled = True
    request_rate_seconds = 0.005  # minimum seconds between requests
    # request_rate_seconds = 0.000
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
        if self.rate_limit_enabled and delta < self.request_rate_seconds:
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
        import traceback
        # print(cmd, resp)
        # traceback.print_stack()
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
        """
        See https://github.com/qemu/qemu/blob/5e05c40ced78ed9a3c25a82ec1f144bb7baffe3f/monitor/misc.c#L615
        :param addr:
        :param size:
        :return:
        """
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
        """
        See https://github.com/qemu/qemu/blob/5e05c40ced78ed9a3c25a82ec1f144bb7baffe3f/monitor/misc.c#L664
            https://github.com/qemu/qemu/blob/5e05c40ced78ed9a3c25a82ec1f144bb7baffe3f/monitor/misc.c#L635
        :param addr:
        :return:
        """
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
        """
        See https://github.com/qemu/qemu/blob/5e05c40ced78ed9a3c25a82ec1f144bb7baffe3f/monitor/misc.c#L684
        :param addr:
        :return:
        """
        cmd = {
            "execute": "human-monitor-command",
            "arguments": {"command-line": "gva2gpa {}".format(addr)}
        }
        # print('Getting guest physical address of guest virtual address {}'.format(hex(addr)))
        response = self.run_cmd(cmd)
        # print(cmd, response)
        lines = response['return'].replace('\r', '').split('\n')
        data_string = ' '.join(l.partition('gpa: ')[2] for l in lines).strip()
        try:
            data = int(data_string, 16)
        except ValueError:
            print(f'Error converting gpa {hex(addr)} to gva (got {response})')
            raise
        return data

    def gva2hva(self, addr):
        return self.gpa2hva(self.gva2gpa(addr))

    def translate(self, addr):
        return self.gva2hva(addr)


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


"""
known_addresses is a map of guest address to host address translations

it has the form:
    known_addresses = {
                        <int guest address>: {
                             'host_address': <int host address>
                             'value': <int value>
                             'type': <str value type>
                         }
                      }
"""
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

# stores start time of current game
# this will eventually be replaced by a full game class
game_meta = {}

memory_cache = {}


def populate_memory_cache():
    """
    This caching layer is just a way to store snapshots of large segments of contiguous memory for future lookups,
    rather than using many small pymem lookups against live memory. This cache should be invalidated and repopulated
    every tick by calling invalidate_memory_cache()

    memory_cache consists of a dict where the keys are tuples of the form
        (start address, end address, host address)
    :return:
    """

    # game state
    game_state_base_address = read_u32(0x2E2D14)
    game_state_size = read_u32(0x32E4A)
    host_address = get_host_address(game_state_base_address)
    memory_cache[(game_state_base_address, game_state_base_address + game_state_size, host_address)] = \
        read_bytes(game_state_base_address, game_state_size)

    # spawns from tags cache
    global_scenario_address = read_u32(0x39BE5C)
    first_spawn_address = read_s32(global_scenario_address + 856)
    if first_spawn_address:
        spawn_count = read_s32(global_scenario_address + 852)
        memory_cache[(first_spawn_address, first_spawn_address + 52 * spawn_count, get_host_address(first_spawn_address))] = \
            read_bytes(first_spawn_address, 52 * spawn_count)

    # observer camera
    observer_camera_address = 0x271550
    observer_camera_size = 688 * 4
    host_address = get_host_address(observer_camera_address)
    memory_cache[(observer_camera_address, observer_camera_address + observer_camera_size, host_address)] = \
        read_bytes(observer_camera_address, observer_camera_size)


def invalidate_memory_cache():

    memory_cache.clear()


memory_functions = {
    '<B': pm.read_uchar,
    '<H': pm.read_ushort,
    '<I': pm.read_uint,
    '<Q': pm.read_ulonglong,
    '<c': pm.read_char,
    '<h': pm.read_short,
    '<i': pm.read_int,
    '<f': pm.read_float,
    'bytes': pm.read_bytes,
    'string': pm.read_string,
}

struct_objects = {
    '<B': struct.Struct('<B'),
    '<H': struct.Struct('<H'),
    '<I': struct.Struct('<I'),
    '<Q': struct.Struct('<Q'),
    '<c': struct.Struct('<c'),
    '<h': struct.Struct('<h'),
    '<i': struct.Struct('<i'),
    '<f': struct.Struct('<f'),
}


def read_from_cache(address, fmt, length=128, **kwargs):
    """
    Returns an empty dict if address not found in cache.
    If address is found, returns a dict of the form:
        {
            value,
            host_address,
        }

    TODO: using Box (or similar) for dicts would make this look nicer
            https://github.com/cdgriffith/Box

    # TODO: https://github.com/mborgerson/pyxbe/blob/master/xbe/__init__.py

    :param address:
    :param fmt:
    :param length:
    :param kwargs:
    :return:
    """

    result = {}
    for (start, end, host_address), cached_bytes in memory_cache.items():
        if start <= address <= end:
            offset = address - start
            result['host_address'] = get_host_address(start) + offset
            if fmt not in ['bytes', 'string']:
                # use precompiled structs for performance
                # TODO: check if there's any performance benefit here
                if fmt in struct_objects:
                    result['value'] = struct_objects[fmt].unpack_from(cached_bytes, offset)[0]
                else:
                    # TODO: check if structs are already cached when calling unpack_from like this
                    #   https://docs.python.org/3/library/struct.html#struct.Struct
                    #   https://bugs.python.org/issue42836
                    #   https://github.com/python/cpython/blob/f4c03484da59049eb62a9bf7777b963e2267d187/Modules/_struct.c#L2259
                    #   https://github.com/python/cpython/blob/f4c03484da59049eb62a9bf7777b963e2267d187/Modules/_struct.c#L2110
                    result['value'] = struct.unpack_from(fmt, cached_bytes, offset)[0]
            elif fmt == 'bytes':
                result['value'] = cached_bytes[offset:offset+length]
            elif fmt == 'string':
                buff = cached_bytes[offset:offset+(length if length else 128)]
                i = buff.find(b'\x00')
                if i != -1:
                    buff = buff[:i]
                result['value'] = buff.decode()
    return result


def get_host_address_from_cache(address):

    for (start, end, host_address), cached_bytes in memory_cache.items():
        if start <= address <= end:
            offset = address - start
            return get_host_address(start) + offset

    return -1


# FIXME: avoid the forced qmp lookup in get_host_address
def get_host_address(address):

    if address in known_addresses:
        return known_addresses[address]['host_address']
    # FIXME: infinite recursion possible here?
    #        shouldn't be because we're calling get_host_address() of base addresses when we initialize the cache
    elif (host_address := get_host_address_from_cache(address)) >= 0:
        known_addresses[address]['host_address'] = host_address
        return host_address
    else:
        host_address = t.translate(address)
        known_addresses[address]['host_address'] = host_address
        # print(f'cache miss: {hex(address)} -> {hex(host_address)}')
        return host_address


def read_memory(address, fn, retry_on_value_change=False, is_host_address=False, watch=False, return_host_address=False, assume_contiguous_ram=True, **kwargs):
    """
    The first time a guest (xbox) address is read, store its translated host address.
    Future read attempts of that guest address will just use the stored host address.

    FIXME: I don't like the way I'm passing function handles into this function. Replace with something better.

    Note: I tried out PyMeow (1.4) as a replacement for pymem, but it ended up being roughly 3x slower
            (~90ms per game_update() with pymeow vs ~30ms with pymem, tested with ~1900 read operations per game_update)
            https://github.com/qb-0/PyMeow

    TODO: try volatility for memory access:
            https://github.com/volatilityfoundation/volatility3
            https://github.com/koromodako/volatility  (python3)

    See game_state_initialize() and physical_memory_allocate() for contiguous physical region allocation
    Game state buffer starts at 0x80061000
    It takes pymem ~4ms to read the entire game state region, and ~21ms to read the tag cache region.

    :param assume_contiguous_ram:
    :param return_host_address:
    :param fn:
    :param address:
    :param retry_on_value_change:
    :param is_host_address:
    :param watch:
    :return:
    """

    # FIXME: would be nice to have a central stats location for counters
    global pymem_counter

    if is_host_address:
        value = memory_functions[fn](address=address, **kwargs)
        pymem_counter += 1

    # read from cached bytestring if this address is in a cached segment
    elif memory_cache and (cached_value := read_from_cache(address, fn, **kwargs)):
        value = cached_value['value']
        known_addresses[address]['value'] = value
        known_addresses[address]['host_address'] = cached_value['host_address']
        known_addresses[address]['type'] = memory_functions[fn].__name__

    # use pymem to read from live memory if we've already translated this guest address to a host address
    elif address in known_addresses:

        # read value from host memory address if we've already translated the guest address
        value = memory_functions[fn](address=known_addresses[address]['host_address'], **kwargs)
        pymem_counter += 1

        # if value has changed and it should not have changed, translate address again and retry
        if retry_on_value_change and value != known_addresses[address]['value']:
            print(f'WARNING: value for {hex(address)} has changed from {hex(known_addresses[address]["value"])} to {hex(value)}')
            known_addresses[address]['host_address'] = t.gva2hva(address)

            # TODO: should we reread the value using the new host address?
            value = memory_functions[fn](address=known_addresses[address]['host_address'], **kwargs)
            pymem_counter += 1

        known_addresses[address]['value'] = value
        known_addresses[address]['type'] = memory_functions[fn].__name__

    # the 0x80000000+ guest region appears to always be laid out in a contiguous segment of host memory in xemu
    # TODO: only relevant if we're not caching contiguous segments in memory_cache, can probably just remove this
    elif assume_contiguous_ram and address > 0x80000000:
        base_address = get_host_address(0x80000000)
        offset = address - 0x80000000
        host_address = base_address + offset
        known_addresses[address]['host_address'] = host_address  # FIXME: should we actually store this if it's a guess?
        value = memory_functions[fn](address=host_address, **kwargs)
        pymem_counter += 1
        known_addresses[address]['value'] = value
        known_addresses[address]['type'] = memory_functions[fn].__name__

    # translate guest address to host address if this is the first time we're seeing the guest address
    else:
        known_addresses[address]['host_address'] = t.gva2hva(address)
        value = memory_functions[fn](address=known_addresses[address]['host_address'], **kwargs)
        pymem_counter += 1
        known_addresses[address]['value'] = value
        known_addresses[address]['type'] = memory_functions[fn].__name__
        # value = int.from_bytes(t.read(address, 4), 'little')

    # print(f'{hex(address)} -> {hex(watched_addresses[address]["host_address"]) if address in watched_addresses else ""}: {hex(value)}')
    return value


def read_u8(address, *args, **kwargs):
    return read_memory(address, '<B', *args, **kwargs)


def read_u16(address, *args, **kwargs):
    return read_memory(address, '<H', *args, **kwargs)


def read_u32(address, *args, **kwargs):
    return read_memory(address, '<I', *args, **kwargs)


def read_u64(address, *args, **kwargs):
    return read_memory(address, '<Q', *args, **kwargs)


def read_s8(address, *args, **kwargs):
    return read_memory(address, '<c', *args, **kwargs)


def read_s16(address, *args, **kwargs):
    return read_memory(address, '<h', *args, **kwargs)


def read_s32(address, *args, **kwargs):
    return read_memory(address, '<i', *args, **kwargs)


def read_float(address, *args, **kwargs):
    return read_memory(address, '<f', *args, **kwargs)


def read_bytes(address, length, *args, **kwargs):
    return read_memory(address, 'bytes', length=length, *args, **kwargs)


def read_string(address, length=128, *args, **kwargs):
    return read_memory(address, 'string', byte=length, *args, **kwargs)


player_datum_array = read_u32(0x2FAD28)
player_datum_array_max_count = read_u16(player_datum_array + 0x20)
player_datum_array_element_size = read_u16(player_datum_array + 0x22)
player_datum_array_first_element_address = read_u32(player_datum_array + 0x34)

# t.gpa2hva(0x2FAD20)
# gpa = t.gva2gpa(player_datum_array_first_element_address)
# hva = t.gpa2hva(gpa)
# hpa = t.gpa2hpa(gpa)

pprint(dict(
    player_datum_array=hex(player_datum_array),
    player_datum_array_max_count=player_datum_array_max_count,
    player_datum_array_element_size=player_datum_array_element_size,
    player_datum_array_first_element_address=hex(player_datum_array_first_element_address)
))

players_globals_address = read_u32(0x2FAD20)
teams_address = read_u32(0x2FAD24)
game_globals_address = read_u32(0x27629C)
# game_globals_address = read_u32(0x39BE4C)  # FIXME: this doesn't seem like the correct address... but need to know what it is
game_server_address = read_u32(0x2E3628)
game_client_address = read_u32(0x2E362C)
# game_connection_word = read_u16(0x2E3684)
game_connection_address = 0x2E3684
is_team_game_address = read_u8(0x2F90C4)
game_time_globals_address = read_u32(0x2F8CA0)
global_tag_instances_address = read_u32(0x39CE24)
# game_globals_276 = read_u32(game_globals_address + 276)
# game_globals_276_108 = read_u16(game_globals_address + 108)
hud_messages_pointer = read_u32(0x276B40)

# network game server
# total_players = read_u16(game_server_address + 0x224)
# max_players = read_u16(game_server_address + 0x10E)
# print('total players: {}'.format(total_players))
# print('max players: {}'.format(max_players))

something_saying_main_menu = read_u32(0x2E4000 + 4)


def get_spawns():

    global_scenario_address = read_u32(0x39BE5C)
    spawn_count = read_u32(global_scenario_address + 852)
    first_spawn_address = read_u32(global_scenario_address + 856)

    spawns = []

    if spawn_count > 0:
        for spawn_index in range(spawn_count):
            spawn_address = first_spawn_address + 52 * spawn_index
            spawn = dict(
                address=f'{hex(spawn_address)} -> {hex(get_host_address(spawn_address))}',
                spawn_id=spawn_index,
                x=read_float(spawn_address),
                y=read_float(spawn_address + 4),
                z=read_float(spawn_address + 8),
                facing=read_float(spawn_address + 12),
                team_index=read_u8(spawn_address + 16),
                bsp_index=read_u8(spawn_address + 17),
                unk0=hex(read_u16(spawn_address + 18)),
                gametypes=[
                    read_u8(spawn_address + 20),
                    read_u8(spawn_address + 21),
                    read_u8(spawn_address + 22),
                    read_u8(spawn_address + 23),
                ]
            )
            spawns.append(spawn)

    return spawns


def get_items():

    # TODO: detect when a new map is loaded (pregame) and preload all its items, to avoid qmp lookups on first tick

    # print('====================')
    global_scenario_address = read_u32(0x39BE5C)

    # from game_engine_update_item_spawn()
    item_count = read_s32(global_scenario_address + 900)
    first_item_address = read_u32(global_scenario_address + 904)
    # print(f'{item_count} items starting at {hex(first_item_address)} -> {hex(t.translate(first_item_address))}')
    items = []
    # return items
    if item_count > 0:
        for item_index in range(item_count):
            # print('=========ITEM START=========')
            item_address = first_item_address + 144 * item_index
            unknown_item_attribute = read_s16(item_address + 0xE)  # see v4 in game_engine_update_item_spawn from cache.exe
            if True:
            # if not unknown_item_attribute:
                tag_index = read_s32(item_address + 0x5C)  # example: -0x1b6cfce1 (-460127457) -> 0x31f (799) after & 0xFFFF
                # tag_index_u = read_u32(item_address + 0x5C)
                if tag_index != -1:
                    # print(f'item {item_index}: {tag_index} {hex(tag_index)} -> {hex(tag_index & 0xFFFF)} {tag_index_u & 0xFFFF}')
                    # print(hex(tag_index), '|', tag_index, '|', hex(tag_index_u), '|', tag_index_u)
                    # # print(hex(tag_index_u & 0xFFFF))
                    # # print(32 * (tag_index & 0xFFFF) + global_tag_instances_address + 0x14)
                    # tag_instance_address = global_tag_instances_address + 32 * (tag_index & 0xFFFF)
                    # print(f'tag_instance_address: {hex(tag_instance_address)} -> {hex(t.translate(tag_instance_address))}')
                    # same_tag_index_as_above = read_s32(tag_instance_address + 0xC)
                    # some_other_address = read_u32(tag_instance_address + 0x10)
                    # print(f'some_other_address: {hex(some_other_address)} -> {hex(t.translate(some_other_address))}')
                    # tag_definition_address = read_u32(tag_instance_address + 0x14)
                    # print(f'tag_definition_address: {hex(tag_definition_address)} -> {hex(t.translate(tag_definition_address))}')
                    # print(tag_index, same_tag_index_as_above)

                    tag_name = read_string(read_s32(global_tag_instances_address + 32 * (tag_index & 0xFFFF) + 0x10))
                    item_spawn_interval = read_s16(read_s32(global_tag_instances_address + 32 * (tag_index & 0xFFFF) + 0x14) + 0xC)
                    # something = read_bytes(read_s32(global_tag_instances_address + 32 * (tag_index & 0xFFFF) + 0x14), 14)
                    # item_ptr_address = read_u32(read_s32(global_tag_instances_address + 32 * (tag_index & 0xFFFF) + 0x14) + 0x4)
                    # actual_item = read_bytes(item_ptr_address, 32)
                    # print(item_spawn_interval, hex(item_spawn_interval), something)
                    # print(item_spawn_interval, item_ptr_address, hex(item_ptr_address), '->', actual_item)
                    # print(item_spawn_interval, '---', read_bytes(read_s32(global_tag_instances_address + 32 * (tag_index & 0xFFFF) + 0x14), 32))#.decode("utf-8", 'ignore'))
                    # if tag_name != 'cmti':
                    #     print(tag_name)
                    # print(read_u32(global_tag_instances_address + 32 * (tag_index & 0xFFFF) + 0x10)).decode("utf-8", 'ignore')

                    # TODO: also check item_get_position_even_if_in_inventory()
                    item = dict(
                        address=f'{hex(item_address)} -> {hex(get_host_address(item_address))}',
                        tag_id=tag_index & 0xFFFF,
                        tag_name=tag_name,
                        item_spawn_interval=item_spawn_interval,
                        item_game_type=read_u8(item_address + 0x4),
                        item_x=read_float(item_address + 0x40),
                        item_y=read_float(item_address + 0x44),
                        item_z=read_float(item_address + 0x48),
                    )
                    items.append(item)
    return items


last_game_connection = ''
last_game_in_progress = (0,0,0)


def get_game_time_info():
    """
    Players first spawn in on tick 0.
    The game logic for the nth tick happens while game_time is set to n, and game_time is only increased at the end of
    the tick (before rendering starts).
    :return:
    """

    # TODO: use this as a test for struct unpack (read these 32 bytes all at once instead of multiple memory reads)
    #       or ctypes.LittleEndianStructure with from_buffer_copy()
    #       see https://github.com/mborgerson/pyxbe/blob/master/xbe/__init__.py

    game_time_info = dict(
        game_time_globals_address=game_time_globals_address,
        game_time_initialized=read_u8(game_time_globals_address),
        game_time_active=read_u8(game_time_globals_address + 1),
        game_time_paused=read_u8(game_time_globals_address + 2),
        game_time=read_u32(game_time_globals_address + 12) - 1,  # gets incremented after game engine is done, so we really want game_time-1
        game_time_elapsed=read_u32(game_time_globals_address + 16),
        game_time_speed=read_float(game_time_globals_address + 24),  # 1.0 is normal speed
        game_time_leftover_dt=read_float(game_time_globals_address + 28),
        update_client_maximum_actions=read_u32(0x2E87E8) - read_u32(0x2E87E4) + 1,  # typically gets set to 1 then decremented back to 0
        game_time_globals_address_hex=f'{game_time_globals_address:#x} -> {known_addresses[game_time_globals_address]["host_address"]:#x}',
        real_time_elapsed=str(datetime.timedelta(seconds=read_u32(game_time_globals_address + 12)/30)).split('.')[0],  # FIXME duplicated read
    )

    return game_time_info


def get_hud_message(message_index):

    return read_string(hud_messages_pointer + 0x460 * message_index)


def object_string_from_type(object_type):

    object_type_definitions_array = 0x1FCB78
    type_def_addr = read_u32(object_type_definitions_array + 4 * object_type)
    type_string = read_string(read_u32(type_def_addr))
    return type_string


def get_objects():
    """
    Every 30 seconds, the object header table gets rearranged

    # FIXME: weapons held by players show up as static objects when the player switches weapons if we blindly use xyz

    object_iterator_new() types: (also see object_try_and_get_and_verify_type())
        -1 - all
         1 - ? in game_engine_update_purge()
         2 - vehicle
         3 - vehicle seat? unit_seat_filled()
        28 - weapon, item
        32 - projectile
       896 - device group?




    :return:
    """

    objects = []
    object_header_datum_array = read_u32(0x2FC6AC)
    # object_header_datum_array_max_elements = read_u16(object_header_datum_array + 0x20)
    # object_header_datum_array_element_size = read_u16(object_header_datum_array + 0x22)
    object_header_datum_array_total_count = read_u16(object_header_datum_array + 0x2E)
    # object_header_datum_array_real_count = read_u16(object_header_datum_array + 0x30, is_host_address=skip_lookups)
    # print(f'{object_header_datum_array_real_count}/{object_header_datum_array_total_count} objects')
    object_header_datum_array_first_element_address = read_u32(object_header_datum_array + 0x34)

    for i in range(object_header_datum_array_total_count):
        # read_u32(object_header_datum_array + 52) + 12 * (weapon_object_handle & 0xFFFF) + 8)
        # tag_address = 32 * read_s16(weapon_object_address) + global_tag_instances_address
        object_address = read_u32(object_header_datum_array_first_element_address + 12 * i + 8)
        if object_address != 0x0:
            tag_name = read_string(read_u32(32 * read_s16(object_address) + global_tag_instances_address + 0x10))
            object_type = read_u8(object_address + 0x64)
            objects.append(dict(
                object_id=i,
                address=f'{hex(object_address)} -> {hex(known_addresses[object_address]["host_address"])}',
                x=read_float(object_address + 0xC),
                y=read_float(object_address + 0x10),
                z=read_float(object_address + 0x14),
                time_existing=read_s16(object_address + 0x6C),
                unk_damage_1=read_s16(object_address + 0x68),
                owner_unit_ref=hex(read_u32(object_address + 0x70)),  # from projectile_collision() damage section
                owner_object_ref=hex(read_u32(object_address + 0x74)),  # from projectile_collision() damage section
                parent_ref=hex(read_u32(object_address + 0xCC)),  # for held weapons, changes to 0xffffffff when in backpack
                # owner=read_u32(object_address + 0x1E0),
                # owner_hex=hex(read_u32(object_address + 0x1E0)),
                state_flags=read_u8(object_address + 0x1A4),  # 3 if held in inventory, 4 if dropping/moving, 8 if stationary on floor
                drop_time=read_u32(object_address + 0x1B4),  # object gets deleted in game_engine_update_purge() after 30 seconds
                object_type=object_type,
                object_type_string=object_string_from_type(object_type),
                tag_name=tag_name,
            ))

    return objects


def get_flag_data():
    game_engine_globals_address = read_u32(0x2F9110)
    if game_engine_globals_address and read_u32(game_engine_globals_address + 0x4) == 1:
        flag_0 = read_u32(0x2762A4)
        flag_1 = read_u32(0x2762A4 + 4)
        return dict(
            flag_base_0=dict(
                x=read_float(flag_0),
                y=read_float(flag_0+4),
                z=read_float(flag_0+8),
            ),
            flag_base_1=dict(
                x=read_float(flag_1),
                y=read_float(flag_1+4),
                z=read_float(flag_1+8),
            ),
        )
    return {}


def vector_3d_from_euler_angles_2d(euler_x, euler_y):

    x = math.cos(euler_x) * math.cos(euler_y)
    y = math.sin(euler_x) * math.cos(euler_y)
    z = math.sin(euler_y)
    return x, y, z


class GameState:
    """
    Primarily used for tracking game state changes which cannot be determined by looking at a single tick's data
    """

    players = []
    damage_table = []

    def _new_game(self):

        # create new players and reset stats
        pass


def get_memory_info():

    memory_info = dict(
        game_state_base_address=f'{hex(read_u32(0x2E2D14))} -> {hex(get_host_address(read_u32(0x2E2D14)))}',
        tag_cache_base_address=f'{hex(read_u32(0x2E2D18))} -> {hex(get_host_address(read_u32(0x2E2D18)))}',
        texture_cache_base_address=f'{hex(read_u32(0x2E2D1C))} -> {hex(get_host_address(read_u32(0x2E2D1C)))}',
        sound_cache_base_address=f'{hex(read_u32(0x2E2D20))} -> {hex(get_host_address(read_u32(0x2E2D20)))}',
        game_state_size=f'{hex(read_u32(0x32E4A))}',
        tag_cache_size=f'{hex(read_u32(0x32E5D))}',
        texture_cache_size=f'{hex(read_u32(0x32E75))}',
        sound_cache_size=f'{hex(read_u32(0x32E8A))}',
    )
    
    # ttt = datetime.datetime.now()
    # read_bytes(read_u32(0x2E2D18), read_u32(0x32E5D))
    # print(datetime.datetime.now() - ttt)
    # ttt = datetime.datetime.now()
    # read_bytes(read_u32(0x2E2D14), read_u32(0x32E4A))
    # print(datetime.datetime.now() - ttt)

    return memory_info


def get_input_data():
    """
    player profile settings:
        +40     button config -- default, southpaw, jumpy, boxer, green thumb
        +41     joystick config -- default, southpaw, legacy, legacy southpaw
        +42     look sensitivity
        +43     invert joystick
        +44     vibration
        +45     invert flight controls
        +46     autocenter

    player_control appears to be between the input layer and the game update layer, and respect validity checks
        (e.g. `change grenade` byte doesn't get set if you only have one type of grenades)

    flow seems to be raw input -> input_abstraction -> player_control -> game updates

    :return:
    """

    player_control_address = read_u32(0x276794)
    player_id = 0

    return dict(
        # player_look_yaw_rates=read_bytes(0x2E4684, 16),
        # player_look_pitch_rates=read_bytes(0x2E4694, 16),
        p0_look_yaw_rate=read_float(0x2E4684),  # these get set to the values from input_abstraction_globals
        p0_look_pitch_rate=read_float(0x2E4694),
        p1_look_yaw_rate=read_float(0x2E4684 + 4),
        p1_look_pitch_rate=read_float(0x2E4694 + 4),
        p2_look_yaw_rate=read_float(0x2E4684 + 8),
        p2_look_pitch_rate=read_float(0x2E4694 + 8),
        p3_look_yaw_rate=read_float(0x2E4684 + 12),
        p3_look_pitch_rate=read_float(0x2E4694 + 12),
        input_abstraction_globals=f'{hex(read_u32(0x2E45A0))} @ {hex(get_host_address(0x2E45A0))}',  # are these indexes into the raw controller input blob?
        player_control_pointer=f'{hex(read_u32(0x276794))} @ {hex(get_host_address(0x276794))}',  # see get_local_player_input_blob() and player_control_test_action*()
        player_control=f'{hex(read_u32(read_u32(0x276794)))} @ {hex(get_host_address(read_u32(0x276794)))}',
        player_facing_horizontal=read_float((player_id << 6) + player_control_address + 0x1C),  # looks like rotation angle in radians, between 0 and 2pi
        player_facing_vertical=read_float((player_id << 6) + player_control_address + 0x20),  # angle in radians between -pi/2 (down) and pi/2 (up)
        player_zoom_level=read_s16((player_id << 6) + player_control_address + 16 + 0x24),  # TODO: remove +16
        player_aim_assist_target=hex(read_u32((player_id << 6) + player_control_address + 16 + 0x28)),
        player_aim_assist_near=read_float((player_id << 6) + player_control_address + 16 + 0x2C),
        player_aim_assist_far=read_float((player_id << 6) + player_control_address + 16 + 0x30),
        input_abstraction_input_state=dict(
            # 28 * local player index
            address=f'{hex(get_host_address(0x2E4600))}',
            # button values are the length of time the button has been held, in ticks, up to 255
            a=read_u8(0x2E4600 + 0x1C * 0 + 0x0),
            black=read_u8(0x2E4600 + 0x1C * 0 + 0x1),
            x=read_u8(0x2E4600 + 0x1C * 0 + 0x2),
            y=read_u8(0x2E4600 + 0x1C * 0 + 0x3),
            b=read_u8(0x2E4600 + 0x1C * 0 + 0x4),
            white=read_u8(0x2E4600 + 0x1C * 0 + 0x5),
            left_trigger=read_u8(0x2E4600 + 0x1C * 0 + 0x6),
            right_trigger=read_u8(0x2E4600 + 0x1C * 0 + 0x7),
            start=read_u8(0x2E4600 + 0x1C * 0 + 0x8),
            back=read_u8(0x2E4600 + 0x1C * 0 + 0x9),
            left_stick_button=read_u8(0x2E4600 + 0x1C * 0 + 0xA),
            right_stick_button=read_u8(0x2E4600 + 0x1C * 0 + 0xB),
            left_stick_vertical=read_float(0x2E4600 + 0x1C * 0 + 0xC),  # up = 1.0, down = -1.0
            left_stick_horizontal=read_float(0x2E4600 + 0x1C * 0 + 0x10),  # left = 1.0, right = -1.0
            right_stick_horizontal=read_float(0x2E4600 + 0x1C * 0 + 0x14),  # left = 1.0, right = -1.0
            right_stick_vertical=read_float(0x2E4600 + 0x1C * 0 + 0x18),  # up = 1.0, down = -1.0
        ),
        input_gamepad_state=dict(
            address=f'{hex(get_host_address(0x276AFC))}',
            address2=f'{hex(get_host_address(0x276A5C))}',
            a=read_u8(0x276A5C + 0x0),
            b=read_u8(0x276A5C + 0x1),
            x=read_u8(0x276A5C + 0x2),
            y=read_u8(0x276A5C + 0x3),
            black=read_u8(0x276A5C + 0x4),
            white=read_u8(0x276A5C + 0x5),
            left_trigger=read_u8(0x276A5C + 0x6),  # 0 to 255, amount held down
            right_trigger=read_u8(0x276A5C + 0x7),
        )
    )


def get_observer_camera_info(local_player_index):

    if local_player_index == -1:
        return {}

    observer_camera_address = 0x271550 + 167 * 4 * local_player_index  # 668 * player

    return dict(
        address=f'{observer_camera_address:#x} -> {get_host_address(observer_camera_address):#x}',
        x=read_float(observer_camera_address),
        y=read_float(observer_camera_address + 4),
        z=read_float(observer_camera_address + 8),
        x_vel=read_float(observer_camera_address + 20),  # NOTE: these are different than player velocities (roughly player_vel * pi?)
        y_vel=read_float(observer_camera_address + 24),
        z_vel=read_float(observer_camera_address + 28),
        x_aim=read_float(observer_camera_address + 32),
        y_aim=read_float(observer_camera_address + 36),
        z_aim=read_float(observer_camera_address + 40),
        fov=read_float(observer_camera_address + 56),  # vertical fov in radians
    )


"""
none: 0,
ctf: 1,
slayer: 2,
oddball: 3,
king: 4,
race: 5,
terminator: 6,
stub: 7,
"""

team_score_addresses_by_gametype = {
    1: 0x2762B4,  # ctf
    2: 0x276710,  # slayer
    3: 0x27653C,  # oddball
    4: 0x2762D8,  # king
    5: 0x2766C8,  # race
}

player_score_addresses_by_gametype = {
    # 1: 0x2762B4,  # ctf player scores are stored in static player object
    2: team_score_addresses_by_gametype[2] + 64,  # slayer
    3: team_score_addresses_by_gametype[3] + 64,  # oddball
    4: team_score_addresses_by_gametype[4] + 64,  # king
    5: team_score_addresses_by_gametype[5] + 64,  # race
}


team_scores=dict(
    ctf_team_score=(read_u32(0x2762B4), read_u32(0x2762B4 + 0x4)),
    ctf_score_limit=read_u32(0x2762BC),
    slayer_team_scores_address=f'{hex(get_host_address(0x276710))}',
    slayer_team_score=(read_u32(0x276710), read_u32(0x276710 + 0x4)),  # TODO: this is an array of 16 scores for ffa
                                                                       #       individual player scores are 16*4 after this address, even in a team game
    slayer_score_limit=read_u32(0x2F90E8),
    oddball_team_score=(read_u32(0x27653C), read_u32(0x27653C + 0x4)),  # TODO: is this an array of 16 scores for ffa?
    oddball_score_limit=read_u32(0x276538),
    king_team_score=(read_u32(0x2762D8), read_u32(0x2762D8 + 0x4)),
    race_team_score=(read_u32(0x2766C8), read_u32(0x2766C8 + 0x4)),
)

cross_tick_stats = {}


def initialize_cross_tick_stats():

    # for each player
    # damage dealt
    # damage received
    # kill times
    # death times
    # camo times
    # overshield times
    pass


def player_score_by_player_id(player_id, gametype):

    # ctf player scores are stored in static player object
    if gametype == 1:
        return 0

    return read_s32(player_score_addresses_by_gametype[gametype] + 4 * player_id)


def team_score_by_team_id(team_id, gametype):

    return read_s32(team_score_addresses_by_gametype[gametype] + 4 * team_id)


def get_game_info():

    player_count = read_u16(player_datum_array + 0x2E)
    player_stat_array = []

    # dict of dicts of the form {<player index dealing damage>: {<player index taking damage>: <damage amount>}}
    damage_counts = defaultdict(dict)

    game_time = read_u32(game_time_globals_address + 12)
    game_time_elapsed = read_u32(game_time_globals_address + 16)
    # print(game_time, game_time_elapsed)

    game_time_initialized = read_u8(game_time_globals_address)
    game_time_active = read_u8(game_time_globals_address + 1)
    game_time_paused = read_u8(game_time_globals_address + 2)
    game_time_speed = read_float(game_time_globals_address + 24)  # 1.0 is normal speed
    game_time_leftover_dt = read_float(game_time_globals_address + 28)

    game_globals_map_loaded = read_u8(game_globals_address)
    game_globals_active = read_u8(game_globals_address + 1)

    main_menu_is_active = read_u8(0x2E4068)

    game_engine_globals_address = read_u32(0x2F9110)

    # game_in_progress
    #   splitscreen
    #   1 1 0 = ingame/postgame/mainmenu
    #   1 0 1 = choose map / pregame / singleplayer paused
    #   1 0 0 = briefly while loading game or changing from postgame to choose map screen
    #   0 0 0 = briefly after singleplayer save and quit (between 110 ingame and 110 main menu)
    global last_game_in_progress,last_game_connection
    if last_game_in_progress != (game_time_initialized, game_time_active, game_time_paused):
        print(f'game in progress changed to {game_time_initialized=} {game_time_active=} {game_time_paused=}')
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

    object_header_datum_array = read_u32(0x2FC6AC)
    object_header_datum_array_max_elements = read_u16(object_header_datum_array + 0x20)
    object_header_datum_array_element_size = read_u16(object_header_datum_array + 0x22)
    object_header_datum_array_allocated_object_count = read_u16(object_header_datum_array + 0x2E)
    object_header_datum_array_element_count = read_u16(object_header_datum_array + 0x30)
    object_header_datum_array_first_element_address = read_u32(object_header_datum_array + 0x34)

    if game_time_initialized and game_time_active and not main_menu_is_active:

        for player_index in range(player_count):

            # looks like this in IDA: *(_DWORD *)(player_data + 52) + 212 * a1;
            static_player_address = player_datum_array_first_element_address + player_index * player_datum_array_element_size

            player_object_handle = read_s32(static_player_address + 0x34)
            previous_player_object_handle = read_s32(static_player_address + 0x38)
            player_object_id = player_object_handle & 0xFFFF

            # *(_DWORD *)(*(_DWORD *)(object_header_data + 52) + 12 * (unsigned __int16)v3 + 8);
            dynamic_player_address = read_u32(object_header_datum_array_first_element_address + (
                        player_object_handle & 0xFFFF) * object_header_datum_array_element_size + 8)

            # print('dynamic player address: {} | {}'.format(hex(dynamic_player_address), dynamic_player_address))
            # print('player_object_handle: {} | {}'.format(hex(player_object_handle), player_object_handle))

            player_object_debug = dict(
                player_object_handle=hex(player_object_handle),
                object_header_datum_array=f'{hex(read_u32(object_header_datum_array))} @ {hex(object_header_datum_array)} -> {hex(known_addresses[object_header_datum_array]["host_address"])}',
                object_header_datum_array_first_element_address=hex(object_header_datum_array_first_element_address),
                dynamic_player_address=f'{hex(dynamic_player_address)} -> {hex(get_host_address(dynamic_player_address))}' if player_object_handle != -1 else "",
                player_object_id=player_object_id,
                static_player_address=f'{hex(static_player_address)} -> {hex(get_host_address(static_player_address))}',
                # object_header_datum_array_max_elements=object_header_datum_array_max_elements,
                # object_header_datum_array_element_size=object_header_datum_array_element_size,
                # object_header_datum_array_allocated_object_count=object_header_datum_array_allocated_object_count,
                # object_header_datum_array_element_count=object_header_datum_array_element_count,
            )

            # see game_statistics_record_kill() for assist logic
            #   track the last 4 damagers
            #   on death, find the max total damage for the damagers who damaged in the past 6 seconds
            #   the assist damage threshold is 40% of that max damage amount
            #
            # NOTE: dynamic player object is unassigned on the same tick as death, so we need to look at the old object
            #       to see the final damage that killed them.
            if player_object_handle == -1:
                damage_table_address = read_u32(object_header_datum_array_first_element_address + (
                        previous_player_object_handle & 0xFFFF) * object_header_datum_array_element_size + 8) + 0x3E0
            else:
                damage_table_address = dynamic_player_address + 0x3E0
            damage_table = []
            for i in range(4):
                damage_time = read_u32(damage_table_address + 16 * i)
                if damage_time != 0xFFFFFFFF:
                    damage_amount = read_float(damage_table_address + 16 * i + 4)
                    static_player = read_u32(damage_table_address + 16 * i + 12)
                    damage_table.append(dict(
                        damage_time=damage_time,
                        damage_amount=damage_amount,

                        # note: dynamic object id doesn't change if the player dies and re-damages with a new object id
                        dynamic_player=read_u32(damage_table_address + 16 * i + 8),
                        static_player=static_player,
                    ))
                    # FIXME: temporary for debug purposes, remove
                    damage_table[-1].update(dict(
                        dynamic_player_hex=hex(damage_table[-1]['dynamic_player']),
                        static_player_hex=hex(damage_table[-1]['static_player'])
                    ))
                    # FIXME: should we exclude overkill damage? (e.g. shooting a rocket at someone with 5 health)
                    last_death = read_u32(static_player_address + 0x84)
                    if player_object_handle != -1 or last_death == game_time - 1:
                        damage_counts[static_player & 0xFFFF][player_index] = damage_amount

            if player_object_handle != -1:

                # FIXME: avoid the forced qmp lookup in get_host_address
                # player_object_debug.update(dynamic_player_address_hex=f'{hex(dynamic_player_address)} -> {hex(get_host_address(dynamic_player_address))}')

                # selected_weapon_handle = read_u32(dynamic_player_address + 4 * read_u16(dynamic_player_address + 0x2A2) + 0x2A8)
                # selected_weapon_address = read_u32(read_u32(object_header_datum_array + 52) + 12 * (selected_weapon_handle & 0xFFFF) + 8)

                r'''
                v6 = *(_DWORD *)(32
                     * (**(_DWORD **)(*(_DWORD *)(object_header_data + 52) + 12 * (unsigned __int16)v5 + 8) & 0xFFFF)
                     + global_tag_instances
                     + 20);
                     
                    70 61 65 77 6D 65 74 69 65 6A 62 6F 6B 01 DF E2 B4 71 3B 80 B4 7B 81 80 00 00 00 00 00 00 00 00
                    \___________________,________________/          |           |
                                  paewmetiejbo                     +16         +20
                 '''
                # selected_weapon_tag_address = 32 * read_s16(selected_weapon_address) + global_tag_instances_address# + 20
                # tag_plus_16 = read_u32(selected_weapon_tag_address + 16)
                # tag_plus_20 = read_u32(selected_weapon_tag_address + 20)

                # selected_weapon_tag_address = read_u32(32 * read_s16(selected_weapon_address) + global_tag_instances_address + 20)

                def get_weapon(weapon_object_handle):
                    """
                    starting weapons owned by players appear to have object ids adjacent to their owners
                        if player is id 28, his weapons are 29 and 30
                        player object ids appear to go 28, 31, 34, ... not sure if this is a strict rule
                        (probably just because they get allocated right after their player is allocated.)
                    :param weapon_object_handle:
                    :return:
                    """

                    # TODO: don't even call get_weapon if we have a 0xFFFFFFFF handle
                    if weapon_object_handle == 0xFFFFFFFF:
                        return {}

                    weapon_object_address = read_u32(read_u32(object_header_datum_array + 52) + 12 * (weapon_object_handle & 0xFFFF) + 8)
                    # TODO: better early exit logic
                    if weapon_object_address == 0x0:
                        return {}
                    tag_address = 32 * read_s16(weapon_object_address) + global_tag_instances_address
                    weapon_type = read_u8(read_u32(tag_address + 20) + 0x309)
                    is_energy_weapon = bool(weapon_type & 8)

                    return dict(
                        # tag_object_id=read_s16(weapon_object_address),
                        # x=read_float(weapon_object_address + 0x50),
                        # y=read_float(weapon_object_address + 0x54),
                        # z=read_float(weapon_object_address + 0x58),
                        heat_meter=read_float(weapon_object_address + 0xD4),  # FIXME: seems to also be used for human weapons, need to figure out what
                        used_energy=read_float(weapon_object_address + 0xE0),  # only if energy weapon
                        charge_amount=read_float(weapon_object_address + 0xF0),  # remaining energy for PR, current overcharge for PP
                        reloading=read_u8(weapon_object_address + 0x258),  # 1 while reloading until reload_time hits 2
                        reload_time=read_s16(weapon_object_address + 0x25A),
                        backpack_ammo_count=read_s16(weapon_object_address + 0x25E),
                        magazine_ammo_count=read_s16(weapon_object_address + 0x260),
                        weapon_tag_address=f'{read_u32(tag_address)} @ {hex(tag_address)} -> {hex(known_addresses[tag_address]["host_address"])}',
                        # owner=read_u32(weapon_object_address + 0x1E0),  # TODO: this isn't really owner, seems to correlate to current action
                        # owner_hex=hex(read_u32(weapon_object_address + 0x1E0)),
                        energy_used=read_float(weapon_object_address + 0x1F0),  # used for whether to delete dropped energy weapon (if == 1.0)
                        weapon_type=weapon_type,  # from weapon_trigger_fire()
                        is_energy_weapon=is_energy_weapon,
                        # tag_plus_16=f'{read_u32(tag_plus_16)} :: {hex(tag_plus_16)} -> {hex(known_addresses[tag_plus_16]["host_address"])}',
                        # tag_plus_20=f'{read_u32(tag_plus_20)} :: {hex(tag_plus_20)} -> {hex(known_addresses[tag_plus_20]["host_address"])}',
                        tag_name=read_string(read_u32(tag_address + 0x10)),
                        object_id=weapon_object_handle & 0xFFFF,
                    )

                # TODO: move this out of get_game_info
                def get_weapons(first_weapon_address):
                    weapons = []
                    for weapon_index in range(4):
                        weapon = get_weapon(read_u32(first_weapon_address + 4 * weapon_index))
                        if weapon:
                            weapons.append(weapon)
                    return weapons

                biped_tag_address = read_u32(
                    32 * (read_u32(dynamic_player_address) & 0xFFFF) + global_tag_instances_address + 0x14)
                biped_camera_height_standing = read_float(biped_tag_address + 0x400)
                biped_camera_height_crouching = read_float(biped_tag_address + 0x404)
                crouchscale = read_float(dynamic_player_address + 0x464)

                player_object_debug['biped_tag_address'] = f'{hex(biped_tag_address)} -> {hex(get_host_address(biped_tag_address))}'

                # TODO: change to dataclasses instead of dicts?
                player_object_data = dict(
                    x=read_float(dynamic_player_address + 0xC),
                    y=read_float(dynamic_player_address + 0x10),
                    z=read_float(dynamic_player_address + 0x14),
                    x_vel=read_float(dynamic_player_address + 0x18),
                    y_vel=read_float(dynamic_player_address + 0x1C),
                    z_vel=read_float(dynamic_player_address + 0x20),
                    legs_pitch=read_float(dynamic_player_address + 0x24),  # legs?
                    legs_yaw=read_float(dynamic_player_address + 0x28),  # legs?
                    legs_roll=read_float(dynamic_player_address + 0x2C),  # legs?
                    pitch1=read_float(dynamic_player_address + 0x30),  # these get set in biped_snap_facing(), not sure what it is. (0, 0, 1) in most cases
                    yaw1=read_float(dynamic_player_address + 0x34),
                    roll1=read_float(dynamic_player_address + 0x38),
                    head_x=read_float(dynamic_player_address + 0x50),  # center point?
                    head_y=read_float(dynamic_player_address + 0x54),
                    head_z=read_float(dynamic_player_address + 0x58),
                    powerup_unk=read_u16(dynamic_player_address + 0x68),  # from player_handle_powerup(), seems to always be 1 (TODO: check other gametypes)
                    powerup_unk2=read_u16(dynamic_player_address + 0x6A),
                    max_health=read_float(dynamic_player_address + 0x88),
                    max_shields=read_float(dynamic_player_address + 0x8C),
                    health=read_float(dynamic_player_address + 0x90),
                    shields=read_float(dynamic_player_address + 0x94),
                    unk_dmg_countdown_0x98=read_float(dynamic_player_address + 0x98),  # starts counting down immediately
                    unk_dmg_countdown_0x9C=read_float(dynamic_player_address + 0x9C),
                    unk_dmg_countdown_0xA4=read_float(dynamic_player_address + 0xA4),  # starts counting down after 2 second delay (after 0xAC counts up to 60), initial value is higher for higher damage amount?
                    unk_dmg_countdown_0xA8=read_float(dynamic_player_address + 0xA8),
                    unk3=read_s32(dynamic_player_address + 0xAC),  # from object_damage_update(), tied to countdowns 0x98 and 0xA4, -1 normally, counts up to ~75 when damaged
                    unk4=read_s32(dynamic_player_address + 0xB0),  # from object_damage_update(), tied to countdowns 0x9C and 0xA8, -1 normally
                    # shields_status_2=hex(read_u16(dynamic_player_address + 0xB2)),
                    shields_charge_delay=read_u16(dynamic_player_address + 0xB4),  # from object_damage_update()

                    # 0x4096 when shields are charging, 0x4112 when overshield charging
                    shields_status=hex(read_u16(dynamic_player_address + 0xB6)),  # 0x0 normally, 0x10 while overshield charging, 0x1000 while shields charging, 0x8 while shields are fully depleted

                    # seems like normal path for players goes to biped_get_sight_position()
                    parent_object=hex(read_s32(dynamic_player_address + 0xCC)),  # e.g. vehicle
                    # unk_camera_0xB6=read_u8(dynamic_player_address + 0xB6),  # both of these are 0 for players, from unit_get_camera_position()
                    # unk_camera_0x64=read_s16(dynamic_player_address + 0x64),

                    camo=read_u8(dynamic_player_address + 0x1B4),  # 65=nocamo (01000001), 81=camo (01010001)
                    flashlight=read_u8(dynamic_player_address + 0x1B6),
                    current_action=read_u32(dynamic_player_address + 0x1B8),    # multi bitfield: some functions only check second byte
                                                                                # 0x0000=no_action
                                                                                # 0x0001=crouch
                                                                                # 0x0002=jump
                                                                                # 0x0008=fire
                                                                                # 0x0010=flashlight    immediately goes back to 0x0 even if held
                                                                                # 0x0440=press_action    cycles back to 0x0 before going to 0x4000
                                                                                # 0x0800=shooting
                                                                                # 0x2fc4=grenade
                                                                                # 0x4000=hold_action
                    xunk0=read_float(dynamic_player_address + 0x1D4),  # unknown, from biped_update_turning(), gets multiplied by leg rotation 24, 28, 2c.
                    yunk0=read_float(dynamic_player_address + 0x1D8),
                    zunk0=read_float(dynamic_player_address + 0x1DC),  # z seems to stay at 0.0, but periodically will briefly flip to same z as others
                    xaima=read_float(dynamic_player_address + 0x1E0),  # unit vectors, -1 to 1 on x y z axes.
                    yaima=read_float(dynamic_player_address + 0x1E4),
                    zaima=read_float(dynamic_player_address + 0x1E8),
                    xaimb=read_float(dynamic_player_address + 0x1EC),
                    yaimb=read_float(dynamic_player_address + 0x1F0),
                    zaimb=read_float(dynamic_player_address + 0x1F4),
                    xaim0=read_float(dynamic_player_address + 0x1F8),  # these seem to be used for projectiles -- see projectile_update()
                    yaim0=read_float(dynamic_player_address + 0x1FC),
                    zaim0=read_float(dynamic_player_address + 0x200),
                    xaim1=read_float(dynamic_player_address + 0x204),  # look in players_update_before_game() and unit_control()
                    yaim1=read_float(dynamic_player_address + 0x208),
                    zaim1=read_float(dynamic_player_address + 0x20C),
                    xaim2=read_float(dynamic_player_address + 0x210),
                    yaim2=read_float(dynamic_player_address + 0x214),
                    zaim2=read_float(dynamic_player_address + 0x218),
                    move_forward=read_float(dynamic_player_address + 0x228),
                    move_left=read_float(dynamic_player_address + 0x22C),
                    move_up=read_float(dynamic_player_address + 0x230),  # not sure if this is used anywhere? banshee controls? observer?
                    animation_1=read_u8(dynamic_player_address + 0x253),  # see unit_update_animation(), 0x253 and 0x254 both seem related to animations (movement, grenade throwing, melee, etc)
                    animation_2=read_u8(dynamic_player_address + 0x254),
                    selected_weapon_index=read_s16(dynamic_player_address + 0x2A2),  # 0 or 1 for primary/secondary, -1 for none, see first_person_weapon_index_from_weapon_index()
                    # selected_weapon_index_2=read_s16(dynamic_player_address + 0x2A4),  # seems to only matter if you fully drop a weapon without picking up a replacement
                    # primary_weapon_object=read_u32(dynamic_player_address + 0x2A8),
                    # secondary_weapon_object=read_u32(dynamic_player_address + 0x2AC),
                    # selected_weapon_object=read_u32(dynamic_player_address + 4 * read_u16(dynamic_player_address + 0x2A2) + 0x2A8),
                    # selected_weapon_object_hex=f'{hex(selected_weapon_handle)} -> {hex(selected_weapon_handle & 0xFFFF)=}',
                    # selected_weapon_address=selected_weapon_address,
                    # selected_weapon_address_hex=f'{read_u32(selected_weapon_address)} @ {hex(selected_weapon_address)} -> {hex(known_addresses[selected_weapon_address]["host_address"])}',
                    # weapons=[get_weapon(read_u32(dynamic_player_address + 0x2A8 + 4 * weapon_index)) for weapon_index in range(4)],
                    weapons=get_weapons(dynamic_player_address + 0x2A8),
                    # weapon_0=get_weapon(read_u32(dynamic_player_address + 0x2A8)),
                    # weapon_1=get_weapon(read_u32(dynamic_player_address + 0x2AC)),
                    # weapon_2=get_weapon(read_u32(dynamic_player_address + 0x2B0)),
                    # weapon_3=get_weapon(read_u32(dynamic_player_address + 0x2B4)),
                    # selected_weapon=get_weapon(read_u32(dynamic_player_address + 4 * read_u16(dynamic_player_address + 0x2A2) + 0x2A8)),
                    primary_nades=read_u8(dynamic_player_address + 0x2CE),
                    secondary_nades=read_u8(dynamic_player_address + 0x2CF),

                    camo_amount=read_float(dynamic_player_address + 0x32C),  # 0=nocamo, 1=fullcamo, from game_engine_player_depower_active_camo()
                    # camo_thing2=read_float(dynamic_player_address + 0x330),  # from first_person_weapon_draw()

                    # 0 normally, 1 when player has camo and is revealed by shooting (but not being shot at)
                    camo_self_revealed=read_u16(dynamic_player_address + 0x3D2),  # from player_powerup_on(), not sure when this actually gets set

                    # see game_statistics_record_kill() and unit_record_damage()
                    damagers_list_address=hex(get_host_address(dynamic_player_address + 0x3E0)),

                    melee_attack_time=read_u8(dynamic_player_address + 0x45D),  # ticks until next action is allowed? (next shot or melee)
                    melee_attack_last_tag_id=read_u8(dynamic_player_address + 0x45E),  # TODO: not sure if this is the tag id -- but only changes on first melee with new weapon

                    crouchscale=crouchscale,

                    # seems like if x or y is greater than z, you start sliding or falling? you can watch it change when slowly walking off a ledge
                    facing1=read_float(dynamic_player_address + 0x46C),  # used in biped_snap_facing, not sure purpose (usually 0,0,1 on flat ground)
                    facing2=read_float(dynamic_player_address + 0x470),  # except when on small ledges? e.g. on flat part of zyos ledge x increases as you get farther from wall
                    facing3=read_float(dynamic_player_address + 0x474),  # on zyos ledge diagonal part the z value starts decreasing from 1. also changes on small depressions in priz floor and ramps

                    # from biped_get_sight_position()
                    camera_x=read_float(dynamic_player_address + 0xC),
                    camera_y=read_float(dynamic_player_address + 0x10),
                    camera_z=(1 - crouchscale) * biped_camera_height_standing + crouchscale * biped_camera_height_crouching + read_float(dynamic_player_address + 0x14),

                    air_1_0x64=read_s16(dynamic_player_address + 0x64),  # any_player_is_in_the_air() and unit_get_camera_position()
                    airborne=read_u8(dynamic_player_address + 0x424),  # &1 = airborne, &2 = slipping, 0 = standing, from biped_update()
                    landing_stun_current_duration=read_u8(dynamic_player_address + 0x428),  # any_player_is_in_the_air(), when you land from a jump, seems to be impact intensity (1 or 2 being flat ground jump, 30 for jumping off top priz fall damage). slowly ramps up to value of 0x429
                    landing_stun_target_duration=read_u8(dynamic_player_address + 0x429),  # biped_start_landing(), looks like the target for 0x428, max of 30?
                    airborne_ticks=read_u8(dynamic_player_address + 0x459),  # biped_flying_through_air(), seems to be number of ticks since leaving ground
                    air_3_0x460=read_s16(dynamic_player_address + 0x460),  # biped_update(), if -1 check for slipping. stays -1 while walking, briefly 0 when landing, 1 if damaged from fall? stays at 0 or 1 until 0x428 reaches 0x429

                    # 0x4096 when shields are charging, 0x4112 when overshield charging
                    air_4_0xB6=read_s16(dynamic_player_address + 0xB6),  # biped_flying_through_air() and unit_get_camera_position(), 8 while shields are damaged from falling or nade, 4096 while shields recharging (from any damage)
                )
            else:

                player_object_data = {}
                # print('player respawns in {} ticks'.format(read_u32(static_player_address + 0x2C)))

            # print(player_object_data['xaim2'], player_object_data['yaim2'], player_object_data['zaim2'])

            # TODO: game_engine_get_state_message()
            # TODO: game_statistics_record_kill() player_data+0xA0

            player_stats = dict(
                player_index=player_index,  # index in the player datum array
                local_player=read_s16(static_player_address + 0x2),  # 0 to 3 if local (controller port), -1 if not local
                name=read_bytes(static_player_address + 0x4, 24).decode('utf-16').split('\x00', 1)[0],
                # is_dead=hex(read_s32(static_player_address + 0xD)),  # from any_player_is_dead() -- value does not change when dead
                # name=t.read(static_player_address + 0x4, 24).decode('utf-16').split('\x00', 1)[0],
                team=read_u32(static_player_address + 0x20),
                action_target=hex(read_u32(static_player_address + 0x24)),  # looks like the object you'll interact with if you press action, set to -1 on spawn
                action=read_u16(static_player_address + 0x28),  # 6 if standing over weapon (7 if only 1 weapon held), 8 if next to vehicle, 0 otherwise, set to 0 on spawn
                respawn_timer=read_u32(static_player_address + 0x2C),
                respawn_penalty=read_u32(static_player_address + 0x30),
                object_ref=hex(read_u32(static_player_address + 0x34)),  # -1 when player is dead
                object_index=read_u16(static_player_address + 0x34),
                object_id=read_u16(static_player_address + 0x36),
                previous_object_ref=hex(read_u32(static_player_address + 0x38)),  #  0x34 gets copied here when player dies
                last_target_object_ref=hex(read_u32(static_player_address + 0x40)),  # set to same as copy above if no target
                time_of_last_shot=read_u32(static_player_address + 0x44),
                player_speed=read_float(static_player_address + 0x6C),
                camo_timer=read_u32(static_player_address + 0x68),
                time_of_last_death=read_u32(static_player_address + 0x84),  # 0 at start of game
                target_player_index=read_u32(static_player_address + 0x88),
                kill_streak=read_u16(static_player_address + 0x92),  # resets to 0 on death
                multikill=read_u16(static_player_address + 0x94),  # resets to 0 on death
                time_of_last_kill=read_s16(static_player_address + 0x96),  # in ticks, resets to -1 on death
                kills=read_s16(static_player_address + 0x98),
                assists=read_s16(static_player_address + 0xA0),
                team_kills=read_s16(static_player_address + 0xA8),
                deaths=read_s16(static_player_address + 0xAA),
                suicides=read_s16(static_player_address + 0xAC),
                score=player_score_by_player_id(player_index, read_u32(game_engine_globals_address + 0x4) if game_engine_globals_address else 0),
                ctf_score=read_s16(static_player_address + 0xC4),
                player_quit=read_u8(static_player_address + 0xD1),  # 1 if player quit, not sure what else
                damage_table=damage_table,
                observer_camera_info=get_observer_camera_info(read_s16(static_player_address + 0x2)),  # TODO: duplicate lookup
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
        process_id=f'{pid} - {hex(pid)}',
        game_type=read_u32(game_engine_globals_address + 0x4) if game_engine_globals_address else '',
        variant=read_u8(0x2F90F4),
        global_stage=read_string(0x2FAC20, length=63),  # only populated for hostbox
        multiplayer_map_name=read_string(0x2E37CD),  # populated for host and join boxes
        # network_game_server=f'{hex(read_u32(read_u32(0x2E3628)))}: {hex(read_u32(read_u32(0x2E3628)))} -> {hex(get_host_address(read_u32(read_u32(0x2E3628))))}',
        # network_game_server_state=read_s16(read_u32(0x2E3628) + 0x4),  # 1 = ingame
                                                                       # 2 = postgame
                                                                       # 0 = picking map?
        game_connection=read_s16(0x2E3684),
        # network_game_client=read_u8(read_u32(0x2E362C)),
        game_engine_has_teams=read_u8(0x2F90C4),
        game_engine_running=game_engine_globals_address != 0,  # true in game and postgame carnage report, false in pregame lobby
        game_engine_can_score=read_u32(0x2FABF0) == 0 and game_engine_globals_address != 0,  # false as soon as you hear "game over"

        # TODO: only look up scores for current gametype
        input_data=get_input_data(),
        flag_data=get_flag_data(),
        local_player_count=read_u16(players_globals_address + 0x24),
        # flag_base_locations=f'{read_float(0x2762A4)} {hex(known_addresses[0x2762A4]["host_address"])}',
        game_time_info=get_game_time_info(),
        observer_cameras_address=f'{get_host_address(0x271550):#x}',
        game_globals_address=f'{hex(game_globals_address)} -> {hex(get_host_address(game_globals_address))}',
        game_globals_map_loaded=game_globals_map_loaded,
        players_are_double_speed=read_u8(game_globals_address + 0x2),
        game_loading_in_progress=read_u8(game_globals_address + 0x3),
        precache_map_status=read_float(game_globals_address + 0x4),
        game_difficulty_level=read_u8(game_globals_address + 0xE),
        game_globals_active=game_globals_active,
        global_random_seed=hex(read_u32(0x2E3648)),
        stored_global_random=hex(read_u32(game_globals_address + 16)),  # gets set to 0xdeadbeef during pregame/mapselect
        main_menu_is_active=main_menu_is_active,
        last_game_in_progress=last_game_in_progress,
        last_game_connection=last_game_connection,
        memory_info=get_memory_info(),
        events=[],
        damage_counts=damage_counts,
        players=player_stat_array,
        objects=get_objects(),
        # items=get_items(),
        spawns=get_spawns(),
        game_ended_this_tick=False,  # this gets set in extract_events()
        current_time=datetime.datetime.now(),
    )

    current_time = game_info['current_time']
    elapsed_time = game_info['game_time_info']['game_time'] + 1
    start_time = current_time - datetime.timedelta(seconds=elapsed_time / 30)
    game_info.update(dict(
        start_time=start_time,
    ))

    if ('start_time' not in game_meta or game_meta['start_time'] is None) and game_info['game_engine_can_score']:
        game_meta['start_time'] = start_time

    if game_info['game_engine_can_score']:
        game_id = f'{game_meta["start_time"].strftime("%Y-%m-%d_%H-%M-%S")}'
    else:
        game_id = ''
    game_info['game_id'] = game_id

    return game_info


def analyze_offset_map():
    """
    Compare guest and host memory offsets to check for contiguous regions

    TODO: make sure guest addresses above 0x80000000 are always contiguous in host memory
    :return:
    """

    memory_map = []
    mismatches = []

    last_guest = 0
    last_host = 0

    for guest, value in sorted(known_addresses.items()):
        host = value['host_address']
        memory_map.append([hex(guest), hex(host), guest - last_guest, host - last_host])
        if guest - last_guest != host - last_host:
            mismatches.append([hex(guest), hex(host), guest - last_guest, host - last_host])
        last_guest = guest
        last_host = host

    print('============= MEMORY MAP =============')
    print('guest, host, guest diff, host diff')
    pprint(memory_map)
    print('============= MISMATCHES =============')
    print('guest, host, guest diff, host diff')
    pprint(mismatches)


def get_game_data():

    team_game_address = 0x2F90C4
    game_engine_address = 0x2F9110
    game_server_address = 0x2E3628
    game_client_address = 0x2E362C
    game_connection_word = 0x2E3684
    players_globals_address = 0x2FAD20
    team_data_address = 0x2FAD24


    # players_globals_is_dead


def send_to_file(data, outfile, compression=''):

    # TODO: better serialization of datetime
    # json.dump(data, outfile, default=str)
    # FIXME: create directories if they don't exist
    if compression:
        data_bytes = json.dumps(data, default=str).encode()
        if compression == 'gz':
            with gzip.open(outfile, 'wb') as f:
                f.write(data_bytes)
        elif compression == 'lz':
            with lzma.open(outfile, 'wb') as f:
                f.write(data_bytes)
        elif compression == 'br':
            with open(outfile, 'wb') as f:
                f.write(brotli.compress(data_bytes))
    else:
        with open(outfile, 'a') as f:
            # f.write(orjson.dumps(data, option=orjson.OPT_INDENT_2 | orjson.OPT_NON_STR_KEYS).decode())
            json.dump(data, f, default=str)
            f.write('\n')


def send_to_database(game_info, db):

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
game_info_queue_for_ui = queue.Queue()


def handle_game_info_loop():
    """
    Continuous loop waiting for new ticks in game_info_queue
    :return:
    """

    # database running locally, use `vagrant up` to start
    try:
        db = DBConnector()
    except:
        print('WARNING: database is not up -- run `vagrant up` to start a local database')

    game_ticks = []
    events = []

    # FIXME: try/catch for common or potential exceptions here -- need to keep this thread alive or restart if it dies

    while True:

        game_info = game_info_queue.get()
        game_id = game_info['game_id']
        # send_to_database(game_info, db)
        # print('inserted into database')

        # game_id only has a value while the game is active
        # FIXME: seems like sometimes the last tick gets skipped or something, causing game_id to get stuck?
        if game_id:

            # pull out the large cross-tick elements so we're not duplicating a ton of data
            events = game_info.pop('events', [])
            spawns = game_info.pop('spawns', [])
            items = game_info.pop('items', [])
            objects = game_info.pop('objects', [])  # FIXME: just temporarily removing this for write performance

            # game_ticks.append(game_info)
            # FIXME: this doesn't handle game crashes or missing the last tick of the game (or probably some other similar cases)
            if game_info['game_ended_this_tick']:
                game = dict(
                    summary=dict(
                        game_id=game_id,
                        is_full_game=game_ticks[0]['game_time_info']['game_time'] == 0,
                        recording_started=game_ticks[0]['current_time'],
                        recording_ended=game_ticks[-1]['current_time'],
                        game_duration_ingame=str(datetime.timedelta(seconds=game_ticks[-1]['game_time_info']['game_time']/30)).split('.')[0],
                        recording_duration=str(game_ticks[-1]['current_time'] - game_ticks[0]['current_time']).split('.')[0],
                        ticks_elapsed=game_ticks[-1]['game_time_info']['game_time'] - game_ticks[0]['game_time_info']['game_time'] + 1,
                        ticks_recorded=len(game_ticks),
                        ticks_dropped=game_ticks[-1]['game_time_info']['game_time'] - game_ticks[0]['game_time_info']['game_time'] + 1 - len(game_ticks),
                    ),
                    events=events,
                    spawns=spawns,
                    items=items,
                    ticks=game_ticks,
                )
                pprint(game['summary'])
                # TODO: do this in a separate process
                send_to_file(game, f'V:/replays/{game_id}_final.json.br', compression='br')
                game_ticks = []
            send_to_file(game_info, f'V:/replays/{game_id}.jsonl')


database_worker_thread = threading.Thread(target=handle_game_info_loop, daemon=True, name='database_thread')
database_worker_thread.start()


default_framerate_address = 0xBB648
refresh_rate_address = 0x1F8C98

game_time_address = game_time_globals_address + 12
# gpa = t.gva2gpa(game_time_address)
# game_time_hva = t.gpa2hva(gpa)
# print(f'game_time: {hex(game_time_address)} -> {hex(game_time_hva)}')
# hva = 0x208081c4624
# print(hva, hex(hva))
# print(f'game_time_globals: {read_u8(game_time_globals_address)} :: {hex(known_addresses[game_time_globals_address]["host_address"])}')
# print(f'first item address: {hex(first_item_address := read_u32(global_scenario_address + 904))} {read_u32(first_item_address)} :: {hex(known_addresses[first_item_address]["host_address"])}')
#
# gpa = t.gva2gpa(game_globals_276)
# global_game_globals_276_hva = t.gpa2hva(gpa)
# print(hex(game_globals_276_108), game_globals_276_108)  # 0???

# game_info = get_game_info()
# for i, player in enumerate(game_info['players']):
#     if player['player_object_debug']['dynamic_player_address']:
#         addr = player['player_object_debug']['dynamic_player_address']
#         print(f"dynamic_player_address {hex(addr)} -> {hex(t.gpa2hva(t.gva2gpa(addr)))}")
#         if i == 0:
#             hva = t.gpa2hva(t.gva2gpa(addr))



# timing tests (calling game_info at the top level of loop), @124 pymem reads per loop:
#  with 2 players:
#   no i/o              30 measurements/tick
#   to_file             20 measurements/tick
#   database             7 measurements/tick
#   db thread read      26 measurements/tick
#   db thread store     24 measurements/tick
#   db+file thread      22 measurements/tick
#   put in queue only   29 measurements/tick
# in practice, game_info will only be called once per tick (as soon as tick changes)
#
# above equals roughly 3700 average pymem reads per tick (111000/second)
# compared to only 8 qmp calls per tick (250/second)
# note: qmp calls are rate limited by adjusting Test.request_rate_seconds
#       with no rate limiting, speed is 25 qmp calls per tick (750/second)

previous_game_info = None

thingy = read_float(0x2714D8)  # the thing that gets subtracted from v4 in observer_pass_time()
#  observed values:
#  0.13333334028720856
#  0.11666667461395264
#  0.10000000894069672
#  0.0833333358168602
#  0.06666667014360428
#  0.05000000447034836
#  0.03333333507180214    <-- idles here

component_1 = read_u8(0x2E36BE)  # 1 unless time is stopped
component_2 = read_float(0x2E3680)  # will always be between 0.0 and 1.0 while ingame

# print(f'{thingy} at {hex(t.gpa2hva(t.gva2gpa(0x2714D8)))} ({component_1} * {component_2})')

# pprint({
#     '0x653525 (0x1F8C95)': read_u8(0x1F8C95),
#     '0x6791E8 (0x2E3660)': read_u64(0x2E3660),  # increases by 2 frames each tick (not really tied to ticks, seems to be frames counted?) -- doesn't get reset on new map.
#                                                 # gets incremented after 0x2e3768, so for a brief period this will have the same value as 0x2e3768
#     '0x679200 (0x2E3678)': read_u64(0x2E3678),  # typically = 0x2E3660 - 2
#     '0x653510 (0x1F8C80)': read_u64(0x1F8C80),  # typically = 0x2E3660 - 3, but in a given tick it seems to take on the relative values -2 (early in tick), -1, -3 (in rapid succession later), in that order
#     '0x2E3678 - 0x1F8C80': read_u64(0x2E3678) - read_u64(0x1F8C80),  # difference between the previous two values
#     '0x2E3688': read_u32(0x2E3688),  # if this != 0, v10 = flt_2E3698, but this looks like it's always 0
# })

# sys.exit()


def matches_gametype(current_gametype: int, gametype_list: list[int]) -> bool:
    """
    Returns True if current_gametype matches any gametypes in gametype_list
        0: none
        1: ctf
        2: slayer
        3: oddball
        4: king
        5: race
        6: terminator
        7: stub
        12: all games
        13: all games except ctf
        14: all games except ctf and race
    :param current_gametype:
    :param gametype_list:
    :return:
    """
    for gametype in gametype_list:
        if (current_gametype == gametype or
                gametype == 12 or
                (gametype == 13 and current_gametype != 1) or
                (gametype == 14 and current_gametype not in (1, 5))):
            return True


def distance(p1: tuple[int, int, int], p2: tuple[int, int, int]) -> float:
    x1, y1, z1 = p1
    x2, y2, z2 = p2
    return (((x2-x1)**2)+((y2-y1)**2)+((z2-z1)**2))**(1/2)


def extract_events(old_game_info: dict, new_game_info: dict) -> list:

    # TODO: if last shot was current tick, check for either a hit or a new projectile owned by this player.
    # TODO: still need to handle the situation where two projectiles hit on the same tick (e.g. slow and fast projectiles from same player)

    events = []

    game_time = new_game_info['game_time_info']['game_time']

    # new game
    if not old_game_info['game_engine_running'] and new_game_info['game_engine_running']:
        events.append(f'{game_time}: New game started on {new_game_info["multiplayer_map_name"]}')
        game_meta['start_time'] = new_game_info['current_time']

    # powerups
    # if new_game_info['players'] and new_game_info['game_engine_can_score']:

    # new damage
    # FIXME: two people can land the finishing shot on the same tick, and both get damage
    #         '66914: .minto damaged Hambone for 25.0',
    #         '66914: Donut damaged Hambone for 25.0',
    #         '66969: .minto damaged Hambone for 25.000003814697266',
    #         '66969: Donut damaged Hambone for 25.0',
    #         '66969: .minto got a kill (5)',
    #         '66969: Hambone died (3)',
    #         '66969: Donut got an assist (2)'
    # FIXME: sometimes final damage isn't counted as an event (maybe skipped or overlapping ticks?)
    # TODO: handle case where player had a charging OS and died from a backwhack (if we exclude OS-invuln-damage)
    if new_game_info['game_engine_can_score']:
        for damage_dealer, damage_receivers in new_game_info['damage_counts'].items():
            damage_dealer_name = new_game_info['players'][damage_dealer]['name']
            if damage_dealer in old_game_info['damage_counts']:
                for damage_receiver, new_amount in damage_receivers.items():
                    damage_receiver_name = new_game_info['players'][damage_receiver]['name']
                    if damage_receiver in old_game_info['damage_counts'][damage_dealer]:
                        old_amount = old_game_info['damage_counts'][damage_dealer][damage_receiver]
                        if new_amount > old_amount:
                            events.append(f'{game_time}: {damage_dealer_name} damaged {damage_receiver_name} for {new_amount - old_amount}')
                    else:
                        events.append(f'{game_time}: {damage_dealer_name} damaged {damage_receiver_name} for {new_amount}')
            else:
                for damage_receiver, new_amount in damage_receivers.items():
                    damage_receiver_name = new_game_info['players'][damage_receiver]['name']
                    events.append(f'{game_time}: {damage_dealer_name} damaged {damage_receiver_name} for {new_amount}')

    # kills, deaths, assists
    if old_game_info['game_engine_running'] and new_game_info['game_engine_running']:
        if len(old_game_info['players']) == len(new_game_info['players']):
            for old_player, new_player in zip(old_game_info['players'], new_game_info['players']):
                if (kills := new_player['kills']) > old_player['kills']:
                    events.append(f'{game_time}: {new_player["name"]} got a kill ({kills})')
                if (deaths := new_player['deaths']) > old_player['deaths']:
                    events.append(f'{game_time}: {new_player["name"]} died ({deaths})')
                if (assists := new_player['assists']) > old_player['assists']:
                    events.append(f'{game_time}: {new_player["name"]} got an assist ({assists})')

    # spawns
    if new_game_info['players'] and new_game_info['game_engine_can_score'] and 'spawns' in new_game_info and new_game_info['spawns']:
        for old_player, new_player in zip(old_game_info['players']
                                              if old_game_info['players']
                                              else [None for _ in new_game_info['players']], # old_game_info has no players on first tick
                                          new_game_info['players']):
            # if player has just spawned
            if not old_player or (not old_player['player_object_data'] and new_player['player_object_data']):
                player_x, player_y, player_z = (new_player['player_object_data']['x'], new_player['player_object_data']['y'], new_player['player_object_data']['z'])
                spawn_found = False
                for spawn in new_game_info['spawns']:
                    """
                        NOTE: player's position can be corrected within the same tick as their spawn (e.g. if spawn is not exactly at floor level)
                        spawn  5.258088111877441, -4.3036417961120605, 3.1934990882873535
                        player 5.258088111877441, -4.3036417961120605, 3.1924989223480225
                               spawn is slightly above floor and player fell to floor on same tick
                        
                        spawn  10.790104866027832, 6.755033016204834, -0.40650099515914917
                        player 10.691853523254395, 6.69184684753418,  -0.3470471501350403   <-- first tick after spawn (d=0.13107470372353)
                        player 10.666483879089355, 6.66647481918335,  -0.4075007438659668,  <-- at rest                (d=0.15207137195678)
                               ^ this is the prisoner twos spawn, which intersects with a sloped wall
                    """
                    # FIXME: I'm working around the above limitation by checking if the spawn is close, but would be nice to be exact...
                    # if player_x == spawn['x'] and player_y == spawn['y'] and (player_z == spawn['z']):
                    d = distance((player_x, player_y, player_z), (spawn['x'], spawn['y'], spawn['z']))
                    if matches_gametype(new_game_info['game_type'], spawn['gametypes']) and d <= 0.2:
                        events.append(f'{game_time}: {new_player["name"]} spawned at spawn id {spawn["spawn_id"]}')
                        spawn_found = True
                        break
                if not spawn_found:
                    events.append(f'{game_time}: {new_player["name"]} spawned at an unknown spawn id ({player_x}, {player_y}, {player_z})')

    # game over
    if old_game_info['game_engine_can_score'] and not new_game_info['game_engine_can_score']:
        events.append(f'{game_time}: Game ended on {new_game_info["multiplayer_map_name"]}')
        game_meta['start_time'] = None
        new_game_info['game_ended_this_tick'] = True

        # we're only generating game ids while the game is active, so make sure the last tick also has an id
        # FIXME: does this mean damage on the game over tick will count? (in cases where last kill happens on different tick than game over)
        new_game_info['game_id'] = old_game_info['game_id']

    return events


def sizeof_fmt(num, suffix="B"):
    for unit in ["", "Ki", "Mi", "Gi", "Ti", "Pi", "Ei", "Zi"]:
        if abs(num) < 1024.0:
            return f"{num:3.1f}{unit}{suffix}"
        num /= 1024.0
    return f"{num:.1f}Yi{suffix}"


def memory_benchmark():

    print('Starting memory benchmark')

    starting_address = 0x80000000
    iterations = 1000
    length = 1024

    for i in range(0, length, 4):
        read_u32(starting_address + i)

    ttt = datetime.datetime.now()
    for _ in range(iterations):
        for i in range(0, length, 4):
            read_u32(starting_address + i)
    test_one = datetime.datetime.now() - ttt
    print(f'   {iterations} iterations of {length} bytes in 4-byte chunks took {test_one} seconds')

    ttt = datetime.datetime.now()
    for _ in range(iterations):
        read_bytes(starting_address, length)
    test_two = datetime.datetime.now() - ttt
    print(f'   {iterations} iterations of one {length} byte chunk ({sizeof_fmt(iterations*length)}) took {test_two} seconds ({test_one/test_two}x faster)')


def main_loop():

    # import dearpygui.dearpygui as dpg
    #
    # dpg.create_context()
    # dpg.create_viewport(title='Custom Title', width=1024, height=768)
    #
    # # with dpg.window(label="Example Window"):
    # #     dpg.add_text("Hello, world")
    # #     dpg.add_button(label="Save")
    # #     dpg.add_input_text(label="string", default_value="Quick brown fox")
    # #     slider = dpg.add_slider_float(label="float", default_value=0.273, max_value=999999999, tag="x")
    #
    # dpg.show_metrics()
    #
    # dpg.setup_dearpygui()
    # dpg.show_viewport()# below replaces, start_dearpygui()
    # # while dpg.is_dearpygui_running():

    # memory_benchmark()

    counter = 0
    global pymem_counter
    # pymem_counter = 0
    last = 0
    last_game_time = 0
    benchmark_tick_count = 0
    benchmark_loop_count = 0
    last_game_info = {}
    events = []
    duration_total = 0

    while True:

        try:

            # action = read_u32(0x2E87E8) - read_u32(0x2E87E4) + 1
            # action = read_u32(0x2E87E8)
            # action = read_float(0x2714D8)
            # action = read_float(0x2E3680)
            # action = pm.read_int(hva + 440)  # dynamic player + 440
            game_time = read_u32(game_time_address) - 1  # game_time is incremented after the tick, so we want time-1
            # game_time = pm.read_uint(game_time_hva)

            benchmark_loop_count += 1

            # benchmark
            # game_info = get_game_info()
            # t.gva2gpa(0x2E3680)

            # print(game_time, counter)
            counter += 1
            if game_time != last_game_time:
                benchmark_tick_count += 1
                real_time = datetime.datetime.now()
                print(f'{real_time} new tick: {game_time} with {counter} measurements and {pymem_counter} pymem reads last tick (avg {benchmark_loop_count/benchmark_tick_count:0.2f} loops/tick)')
                # print(f'{game_info_queue.qsize()} items in queue')
                counter = 0
                pymem_counter = 0

                populate_memory_cache()

                # TODO: only look up things like item locations and gametype stuff once per game
                game_info = get_game_info()

                invalidate_memory_cache()

                # print(pymem_counter)
                if game_info['game_time_info']['game_time'] != game_time:
                    print(f"  WARNING: mismatched game time (expected {game_time}, got {game_info['game_time_info']['game_time']})")

                current = datetime.datetime.now()
                if (duration := (current - real_time).microseconds) > 33000:
                    print(f'  WARNING: this update took longer than one tick: {duration/1000}ms')

                # 19.65ms without caching (with ui)
                # 16.70ms with caching (with ui)
                #
                # 16.08ms without caching (without ui)
                # 15.70ms with caching (without ui)
                # if benchmark_tick_count > 30:
                #     duration_total += duration
                #     print(duration_total/1000/(benchmark_tick_count-30))

                if game_time > last_game_time + 1:
                    print(f'  WARNING: missed {game_time - last_game_time - 1} ticks between {last_game_time} and {game_time}')

                # TODO: move events out of each game_tick. it's only there for convenience of seeing what's going on
                # FIXME: game can actually end on the frame when we start recording. Assuming wontfix
                if last_game_info:

                    # reset events on the tick after game engine stops running
                    #   (it stays running through postgame carnage, and stops during map select)
                    # TODO: this will be handled elsewhere when we move the event list out of game_info
                    if last_game_info['game_engine_running'] and not game_info['game_engine_running']:
                        events = []
                    else:
                        events += extract_events(last_game_info, game_info)
                game_info['events'] = events
                last_game_info = game_info

                # if benchmark_tick_count == 30*300:
                #     analyze_offset_map()
                #     break

                # print full game info for pregame and first tick
                # if game_time < 1:
                #     pprint(game_info)

                # PERF: check performance impact of deepcopying this every time
                # deep copy is needed since we're modifying game_info on the other side of the queue
                game_info_queue.put(copy.deepcopy(game_info))
                game_info_queue_for_ui.put(game_info)
                # items = get_items()
                # pprint(items)

                # analyze_offset_map()

                if clients:

                    data = json.dumps(game_info, default=str)

                    # see this for django channels and websocket throughput:
                    # https://stackoverflow.com/questions/51450136/how-can-i-get-django-channels-to-handle-a-higher-message-frequency
                    # https://stackoverflow.com/questions/67751791/possible-reasons-for-django-channels-intermittently-functioning
                    # https://github.com/WorkShoft/dj-pygame-pong
                    # https://crossbar.io/docs/Adding-Real-Time-to-Django-Applications/
                    # https://stackoverflow.com/questions/67835811/django-channels-does-not-work-with-more-than-20-40-users-in-one-channel

                    # websockets vs webrtc:
                    #   https://discourse.threejs.org/t/html5-multiplayer-games-over-udp-client-server-using-geckos-io/15896/7
                    #   https://w3c.github.io/webtransport/
                    #   https://github.com/geckosio/geckos.io
                    #   if webrtc, look at this in python? https://github.com/xhs/librtcdc
                    # https://news.ycombinator.com/item?id=29651447
                    # look at krunker.io
                    # https://github.com/feross/simple-peer

                    # webtransport:
                    #    https://github.com/aiortc/aioquic
                    #    https://github.com/django/asgiref/issues/280
                    #    https://github.com/aiortc/aioquic/issues/163

                    # TODO: this should also be in a background thread
                    for client in clients:
                        client.sendMessage(data)



                # if game_info['players'] and game_info['players'][1]['player_object_data']:
                #     # health = game_info['players'][1]['player_object_data']['health']
                #     # shields = game_info['players'][1]['player_object_data']['shields']
                #     # print(f'{health=} {shields=}')
                #     data = game_info['players'][1]['player_object_data']
                #     pitch, yaw = data['pitch'], data['yaw']
                #     # x1, y1, z1 = data['xvel'], data['yvel'], data['zvel']
                #     # print(f'{pitch=} {yaw=} {x1=} {y1=} {z1=}')
            # if action != last:
                # print(f'{datetime.datetime.now()} new action: {game_time} {action} after {counter} measurements. {read_u64(0x2E3660)=} ({read_u64(0x2E3660)-game_time*2}), {read_u64(0x1F8C80)=} ({read_u64(0x1F8C80)-game_time*2}), {read_u64(0x2E3678)=} ({read_u64(0x2E3678)-game_time*2})')
                pass
            last_game_time = game_time
            # last = action

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

            # this typically happens when trying to read unmapped memory, e.g. read(0x0)
            pprint(e)
            raise

        except KeyError as e:

            pprint(e)

        except socket.timeout as e:

            print('DROPPED FRAME DUE TO SOCKET TIMEOUT')
            t._qmp.close()
            connect()
            # t._qmp.connect()

        else:

            # dpg.render_dearpygui_frame()
            pass

        # time.sleep(1)
        # time.sleep(0.033)

    # dpg.destroy_context()


def start_ui():

    # TODO: try out 3d stuff https://github.com/hoffstadt/DearPyGui/discussions/1416

    # TODO: compare with https://github.com/pyimgui/pyimgui

    import dearpygui.dearpygui as dpg

    dpg.create_context()
    dpg.create_viewport(title='Xemu Memory Watcher', width=1680, height=1050)

    with dpg.window(label="info"):
        dpg.add_input_text(tag='player_info', width=800, height=900, multiline=True, readonly=True)

    with dpg.window(label="positions", pos=(900, 0)):
        with dpg.plot(label='positions', width=600, height=600):
            dpg.add_plot_axis(dpg.mvXAxis, label="x", tag="x_axis")
            # dpg.set_axis_limits(dpg.last_item(), -40, 40)
            dpg.add_plot_axis(dpg.mvYAxis, label="y", tag="y_axis")
            # dpg.set_axis_limits(dpg.last_item(), -40, 40)
            dpg.add_scatter_series([1], [1], parent="y_axis", tag="series")
            dpg.add_scatter_series([1], [1], parent="y_axis", tag="item_series")
            dpg.add_scatter_series([1], [1], parent="y_axis", tag="object_series")

    # dpg.show_metrics()

    dpg.setup_dearpygui()
    dpg.show_viewport()

    while dpg.is_dearpygui_running():

        try:

            game_info = game_info_queue_for_ui.get(block=False)

            # dpg.set_value('player_info', value=pformat(game_info, sort_dicts=False))

            # orjson gives a roughly 3x speedup here (from 500 loops/tick to 1600 loops/tick)
            # NOTE: OPT_NON_STR_KEYS is needed when damage_counts dict is active (but is slower)
            #       https://github.com/ijl/orjson#opt_non_str_keys
            dpg.set_value('player_info', value=orjson.dumps(game_info, option=orjson.OPT_INDENT_2 | orjson.OPT_NON_STR_KEYS).decode())

            x_positions = []
            y_positions = []

            x_item_positions = []
            y_item_positions = []

            x_object_positions = []
            y_object_positions = []

            if 'items' in game_info and game_info['items']:
                for item in game_info['items']:
                    x_item_positions.append(item['item_x'])
                    y_item_positions.append(item['item_y'])
                dpg.set_value('item_series', [x_item_positions, y_item_positions])
            if 'objects' in game_info and game_info['objects']:
                for obj in game_info['objects']:
                    x_object_positions.append(obj['x'])
                    y_object_positions.append(obj['y'])
                dpg.set_value('object_series', [x_object_positions, y_object_positions])
            if 'players' in game_info and game_info['players']:
                for player in game_info['players']:
                    if player['player_object_data']:
                        x_positions.append(player['player_object_data']['x'])
                        y_positions.append(player['player_object_data']['y'])
                dpg.set_value('series', [x_positions, y_positions])

        except queue.Empty:

            pass

        dpg.render_dearpygui_frame()

    dpg.destroy_context()


if __name__ == '__main__':

    # main_loop()

    # FIXME: PERF: main_loop seems to execute twice as fast as part of main thread (without UI). Maybe try subprocess?
    main_thread = threading.Thread(target=main_loop, daemon=True, name='main_loop_thread')
    main_thread.start()

    start_ui()
