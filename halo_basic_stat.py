import asyncio
import copy
import ctypes
import dataclasses
import gc
import gzip
import json
import lzma
import math
import queue
import re
import threading
import traceback
from collections import defaultdict
from dataclasses import dataclass
from pprint import pprint, pformat

import brotli
import psutil
from pymem.exception import MemoryReadError

import api_client
import ui
import ws_client
# from database import DBConnector
from qmp import QEMUMonitorProtocol
import sys
import os, os.path
import orjson
import subprocess
import time
import socket
import struct
import datetime
import zstandard as zstd
# from memory_mappings_and_offsets import *

from SimpleWebSocketServer import SimpleWebSocketServer, WebSocket

from pymem import Pymem


XEMU_WAIT_INTERVAL_SECONDS = 1
WEBSOCKET_HOST = 'localhost'
WEBSOCKET_PORT = 9000
QMP_HOST = 'localhost'
QMP_PORT = 4444
QMP_RATE_LIMIT_ENABLED = False
QMP_RATE_LIMIT_SECONDS = 0.005
REPLAY_DIRECTORY = 'V:/replays'
REPLAY_COMPRESSION = 'zstd'
COMPRESS_IN_MEMORY = True
API_UPDATE_INTERVAL_SECONDS = 30 * 10
WS_RELAY_ENABLED = True
WS_RELAY_BASE_URL = os.getenv('WS_RELAY_BASE_URL', 'http://127.0.0.1:8787')


def get_pid():
    """Returns the process id of the first xemu instance that has qmp running"""
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


use_pymem = True
pid, pm = None, None


def wait_for_xemu():

    global pid, pm

    pid = None
    while pid is None:

        pid = get_pid()
        if pid is None:
            print(f'waiting {XEMU_WAIT_INTERVAL_SECONDS} more seconds for xemu to start')
            time.sleep(XEMU_WAIT_INTERVAL_SECONDS)
            continue
        print(f'xemu pid is {pid} ({hex(pid)})')
        # pm = Pymem('xemu.exe')
        if use_pymem:
            pm = Pymem()
            pm.open_process_from_id(process_id=pid)
        else:
            from mem_edit import Process
            pm = Process(pid)
        return pm


wait_for_xemu()

clients = []
server = None


class SimpleWSServer(WebSocket):
    def handleConnected(self):
        print('Websocket client connected', self.client, self.address)
        clients.append(self)

    def handleClose(self):
        print('Websocket client disconnected', self.client, self.address)
        clients.remove(self)


def run_websocket_server():
    global server
    server = SimpleWebSocketServer(WEBSOCKET_HOST, WEBSOCKET_PORT, SimpleWSServer,
                                   selectInterval=(1000.0 / 60) / 1000)
    print('Websocket server started', server.serversocket)
    server.serveforever()


server_thread = threading.Thread(target=run_websocket_server, daemon=True, name='websocket_server_thread')
server_thread.start()


class hexdump:
    """
    https://gist.github.com/NeatMonster/c06c61ba4114a2b31418a364341c26c0
    """
    def __init__(self, buf, off=0):
        self.buf = buf
        self.off = off

    def __iter__(self):
        last_bs, last_line = None, None
        for i in range(0, len(self.buf), 16):
            bs = bytearray(self.buf[i : i + 16])
            line = "{:08x}  {:23}  {:23}  |{:16}|".format(
                self.off + i,
                " ".join(("{:02x}".format(x) for x in bs[:8])),
                " ".join(("{:02x}".format(x) for x in bs[8:])),
                "".join((chr(x) if 32 <= x < 127 else "." for x in bs)),
            )
            if bs == last_bs:
                line = "*"
            if bs != last_bs or line != last_line:
                yield line
            last_bs, last_line = bs, line
        yield "{:08x}".format(self.off + len(self.buf))

    def __str__(self):
        return "\n".join(self)

    def __repr__(self):
        return "\n".join(self)


class QmpProxy:
    """
    Interacts with QEMU via QEMU Monitor Protocol (QMP)
    Primarily used here for translating guest addresses to host addresses
    QMP is extremely slow compared to direct memory reads
    """

    last_request_time = datetime.datetime.now()
    rate_limit_enabled = QMP_RATE_LIMIT_ENABLED
    request_rate_seconds = QMP_RATE_LIMIT_SECONDS  # minimum seconds between requests
    # request_rate_seconds = 0.000
    cmd_counter = 0
    cmd_counter_reset = datetime.datetime.now()

    def __init__(self):
        self._qmp = None
        self.connect()

    def connect(self):
        i = 0
        while True:
            print(f'Trying to connect {i}')
            if i > 0:
                time.sleep(1)
            try:
                self._qmp = QEMUMonitorProtocol((QMP_HOST, QMP_PORT))
                self._qmp.connect()
                self._qmp.settimeout(0.5)
            except Exception as e:
                if i > 4:
                    raise
                else:
                    i += 1
                    continue
            break

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
            import traceback
            print(cmd)
            traceback.print_stack()
        resp = self._qmp.cmd_obj(cmd)
        if resp is None:
            raise Exception('Disconnected!')
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

    def is_paused(self):
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


t = QmpProxy()


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

pymem_counter = 0

# stores start time of current game and various cross-tick stats
# this will eventually be replaced by a full game class
game_meta = {}

memory_cache = {}

object_type_datum_sizes = dict(
    
)


def populate_memory_cache():
    """
    This caching layer is just a way to store snapshots of large segments of contiguous memory for future lookups,
    rather than using many small pymem lookups against live memory. This cache should be invalidated and repopulated
    every tick by calling invalidate_memory_cache()

    # TODO: cache segment that includes network data (around 0xD00F12D0)

    memory_cache consists of a dict where the keys are tuples of the form
        (start address, end address, host address)
    :return:
    """

    # game state
    game_state_base_address = read_u32(0x2E2D14)
    game_state_size = read_u32(0x32E4A)
    add_to_cache(game_state_base_address, game_state_size)

    # spawns from tags cache
    global_scenario_address = read_u32(0x39BE5C)
    first_spawn_address = read_s32(global_scenario_address + 856)
    if first_spawn_address:
        spawn_count = read_s32(global_scenario_address + 852)
        add_to_cache(first_spawn_address, 52 * spawn_count)

    # observer camera
    observer_camera_address = 0x271550
    observer_camera_size = 688 * 4
    add_to_cache(observer_camera_address, observer_camera_size)

    # object type definitions
    # object_type_definitions_address = 0x1FCB78
    # object_type_definitions_size = 12 * 4
    # add_to_cache(object_type_definitions_address, object_type_definitions_size)

    # object type definitions
    # TODO: do this once at the start of the game, not every tick
    #       populate a dict of object type names by id
    object_type_definitions_address = 0x1FC0D0
    object_type_definitions_size = (0x1FCBA4 - 0x1FC0D0) * 2  # FIXME: this needs to be longer, we get a type 11 on rockets on bc -- *2 is a workaround
    add_to_cache(object_type_definitions_address, object_type_definitions_size)


def invalidate_memory_cache():

    memory_cache.clear()


def add_to_cache(address, size):

    memory_cache[(address, address + size, get_host_address(address))] = read_bytes(address, size, keep_value=False)


# TODO: clean this up
if use_pymem:
    memory_functions = {
        '<B': pm.read_uchar,
        '<H': pm.read_ushort,
        '<I': pm.read_uint,
        '<Q': pm.read_ulonglong,
        '<b': pm.read_char,
        '<h': pm.read_short,
        '<i': pm.read_int,
        '<f': pm.read_float,
        'bytes': pm.read_bytes,
        'string': pm.read_string,
    }
else:
    memory_functions = {
        '<B': pm.read_memory,
        '<H': pm.read_memory,
        '<I': pm.read_memory,
        '<Q': pm.read_memory,
        '<c': pm.read_memory,
        '<h': pm.read_memory,
        '<i': pm.read_memory,
        '<f': pm.read_memory,
        'bytes': pm.read_memory,
        'string': pm.read_memory,
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
    # FIXME: infinite loop possible here?
    #        shouldn't be because we're calling get_host_address() of base addresses when we initialize the cache
    elif (host_address := get_host_address_from_cache(address)) >= 0:
        known_addresses[address]['host_address'] = host_address
        return host_address
    else:
        host_address = t.translate(address)
        known_addresses[address]['host_address'] = host_address
        # print(f'cache miss: {hex(address)} -> {hex(host_address)}')
        return host_address


def read_memory(address, fn, retry_on_value_change=False, is_host_address=False, keep_value=True, watch=False, return_host_address=False, assume_contiguous_ram=True, **kwargs):
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

    TODO: try mem_edit
            https://mpxd.net/code/jan/mem_edit

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

    # TODO: move this and other counters to game_meta?
    global pymem_counter

    if is_host_address:
        value = memory_functions[fn](address, **kwargs)
        pymem_counter += 1

    # read from cached bytestring if this address is in a cached segment
    elif memory_cache and (cached_value := read_from_cache(address, fn, **kwargs)):
        value = cached_value['value']
        known_addresses[address]['value'] = value if keep_value else 0
        known_addresses[address]['host_address'] = cached_value['host_address']
        known_addresses[address]['type'] = memory_functions[fn].__name__

    # use pymem to read from live memory if we've already translated this guest address to a host address
    elif address in known_addresses:

        # read value from host memory address if we've already translated the guest address
        value = memory_functions[fn](known_addresses[address]['host_address'], **kwargs)
        pymem_counter += 1

        # if value has changed and it should not have changed, translate address again and retry
        if retry_on_value_change and value != known_addresses[address]['value']:
            print(f'WARNING: value for {hex(address)} has changed from {hex(known_addresses[address]["value"])} to {hex(value)}')
            known_addresses[address]['host_address'] = t.gva2hva(address)

            # TODO: should we reread the value using the new host address?
            value = memory_functions[fn](known_addresses[address]['host_address'], **kwargs)
            pymem_counter += 1

        known_addresses[address]['value'] = value if keep_value else 0
        known_addresses[address]['type'] = memory_functions[fn].__name__

    # the 0x80000000+ guest region appears to always be laid out in a contiguous segment of host memory in xemu
    # TODO: only relevant if we're not caching contiguous segments in memory_cache, can probably just remove this
    # FIXME: this assumption does not hold true for things like inbound network packets (around 0xD00F12D0).
    #        Probably need an upper bound here if we're keeping this
    #        Workaround for high memory addresses is to add_to_cache() those segments
    elif assume_contiguous_ram and address > 0x80000000:
        base_address = get_host_address(0x80000000)
        offset = address - 0x80000000
        host_address = base_address + offset
        known_addresses[address]['host_address'] = host_address  # FIXME: should we actually store this if it's a guess?
        value = memory_functions[fn](host_address, **kwargs)
        pymem_counter += 1
        known_addresses[address]['value'] = value if keep_value else 0
        known_addresses[address]['type'] = memory_functions[fn].__name__

    # translate guest address to host address if this is the first time we're seeing the guest address
    else:
        known_addresses[address]['host_address'] = t.gva2hva(address)
        value = memory_functions[fn](known_addresses[address]['host_address'], **kwargs)
        pymem_counter += 1
        known_addresses[address]['value'] = value if keep_value else 0
        known_addresses[address]['type'] = memory_functions[fn].__name__
        known_addresses[address]['qmp'] = True  # TODO: this is for debug purposes, remove this
        known_addresses[address]['qmp_traceback'] = traceback.extract_stack()[-3].line
        # value = int.from_bytes(t.read(address, 4), 'little')

    # print(f'{hex(address)} -> {hex(watched_addresses[address]["host_address"]) if address in watched_addresses else ""}: {hex(value)}')
    return value


if use_pymem:

    def read_u8(address, *args, **kwargs):
        return read_memory(address, '<B', *args, **kwargs)


    def read_u16(address, *args, **kwargs):
        return read_memory(address, '<H', *args, **kwargs)


    def read_u32(address, *args, **kwargs):
        return read_memory(address, '<I', *args, **kwargs)


    def read_u64(address, *args, **kwargs):
        return read_memory(address, '<Q', *args, **kwargs)


    def read_s8(address, *args, **kwargs):
        return read_memory(address, '<b', *args, **kwargs)


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


    def read_wchar(address, length=128, *args, **kwargs):
        return read_bytes(address, length, *args, **kwargs).decode('utf-16').split('\x00', 1)[0]


    def write_bytes(address, value, length, is_guest_address=True, *args, **kwargs):
        if is_guest_address:
            address = get_host_address(address)
        return pm.write_bytes(address, value, length)

else:

    def read_u8(address, *args, **kwargs):
        try:
            return read_memory(address, '<B', read_buffer=ctypes.c_uint8(), *args, **kwargs).value
        except AttributeError:
            return read_memory(address, '<B', read_buffer=ctypes.c_uint8(), *args, **kwargs)

    def read_u16(address, *args, **kwargs):
        try:
            return read_memory(address, '<H', read_buffer=ctypes.c_uint16(), *args, **kwargs).value
        except AttributeError:
            return read_memory(address, '<H', read_buffer=ctypes.c_uint16(), *args, **kwargs)

    def read_u32(address, *args, **kwargs):
        try:
            return read_memory(address, '<I', read_buffer=ctypes.c_uint32(), *args, **kwargs).value
        except AttributeError:
            return read_memory(address, '<I', read_buffer=ctypes.c_uint32(), *args, **kwargs)

    def read_u64(address, *args, **kwargs):
        try:
            return read_memory(address, '<Q', read_buffer=ctypes.c_uint64(), *args, **kwargs).value
        except AttributeError:
            return read_memory(address, '<Q', read_buffer=ctypes.c_uint64(), *args, **kwargs)

    def read_s8(address, *args, **kwargs):
        try:
            return read_memory(address, '<b', read_buffer=ctypes.c_int8(), *args, **kwargs).value
        except AttributeError:
            return read_memory(address, '<b', read_buffer=ctypes.c_int8(), *args, **kwargs)

    def read_s16(address, *args, **kwargs):
        try:
            return read_memory(address, '<h', read_buffer=ctypes.c_int16(), *args, **kwargs).value
        except AttributeError:
            return read_memory(address, '<h', read_buffer=ctypes.c_int16(), *args, **kwargs)

    def read_s32(address, *args, **kwargs):
        try:
            return read_memory(address, '<i', read_buffer=ctypes.c_int32(), *args, **kwargs).value
        except AttributeError:
            return read_memory(address, '<i', read_buffer=ctypes.c_int32(), *args, **kwargs)

    def read_float(address, *args, **kwargs):
        try:
            return read_memory(address, '<f', read_buffer=ctypes.c_float(), *args, **kwargs).value
        except AttributeError:
            return read_memory(address, '<f', read_buffer=ctypes.c_float(), *args, **kwargs)

    def read_bytes(address, length, *args, **kwargs):
        return read_memory(address, 'bytes', read_buffer=(ctypes.c_byte * length)(), *args, **kwargs)

    def read_string(address, length=128, *args, **kwargs):
        return str(read_memory(address, 'string', read_buffer=(ctypes.c_char * length)(), *args, **kwargs).value)


def get_formatted_bytes(address, length, columns=32):

    data = read_bytes(address, length)
    data_string = [data.hex(' ')[i:i+3*columns].strip() for i in range(0, len(data.hex(' ')), 3*columns)]
    return data_string


# FIXME: qmp lookups outside functions
player_datum_array = read_u32(0x2FAD28)
player_datum_array_max_count = read_u16(player_datum_array + 0x20)
player_datum_array_element_size = read_u16(player_datum_array + 0x22)
player_datum_array_first_element_address = read_u32(player_datum_array + 0x34)

players_globals_address = read_u32(0x2FAD20)
teams_address = read_u32(0x2FAD24)
game_globals_address = read_u32(0x27629C)
global_game_globals_address = read_u32(0x39BE4C)
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

spawns_cache = []


def get_spawns(cache_results=True):

    if cache_results and spawns_cache:
        return spawns_cache

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

                # NOTE: in ctf, team_index will be set to 3 if spawn is closer to enemy flag than team flag
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

    if cache_results:
        spawns_cache[:] = spawns

    return spawns


items_cache = []


def get_items(cache_results=True):

    # TODO: detect when a new map is loaded (pregame) and preload all its items, to avoid qmp lookups on first tick

    if cache_results and items_cache:
        return items_cache

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

    if cache_results:
        items_cache[:] = items

    return items


def clear_caches():
    spawns_cache.clear()
    items_cache.clear()


last_game_connection = ''
last_game_in_progress = (0, 0, 0)


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
        game_time_monitor_state=read_s16(game_time_globals_address + 4),
        game_time_monitor_counter=read_s16(game_time_globals_address + 6),
        game_time_monitor_latency=read_s16(game_time_globals_address + 8),
        game_time=read_u32(game_time_globals_address + 12) - 1,  # gets incremented after game engine is done, so we really want game_time-1
        game_time_elapsed=read_u32(game_time_globals_address + 16),  # looks like elapsed time in last drawn frame (dropped frame count)
        game_time_speed=read_float(game_time_globals_address + 24),  # 1.0 is normal speed
        game_time_leftover_dt=read_float(game_time_globals_address + 28),
        update_client_maximum_actions=read_u32(0x2E87E8) - read_u32(0x2E87E4) + 1,  # typically gets set to 1 then decremented back to 0
        game_time_globals_address_hex=f'{game_time_globals_address:#x} -> {known_addresses[game_time_globals_address]["host_address"]:#x}',
        real_time_elapsed=str(datetime.timedelta(seconds=read_u32(game_time_globals_address + 12)/30)).split('.')[0],  # FIXME duplicated read
    )

    return game_time_info


def get_key_data():

    return dict(
        kernel_header=get_formatted_bytes(0x80010000, 200),
        # data=get_formatted_bytes(0x80060220, 0x80060380 - 0x80060220)
    )


def get_hud_message(message_index):

    return read_string(hud_messages_pointer + 0x460 * message_index)


def object_string_from_type(object_type):

    object_type_definitions_array = 0x1FCB78
    type_def_addr = read_u32(object_type_definitions_array + 4 * object_type)
    type_string = read_string(read_u32(type_def_addr))
    return type_string

    
def datum_size_from_object_type(object_type):

    object_type_definitions_array = 0x1FCB78
    type_def_addr = read_u32(object_type_definitions_array + 4 * object_type)
    datum_size = read_u16(type_def_addr + 8)
    # print(get_formatted_bytes(type_def_addr, 24))
    return datum_size


def get_objects():
    """
    Every 30 seconds, the object header table gets rearranged

    # FIXME: weapons held by players show up as static objects when the player switches weapons if we blindly use xyz

    TODO: should this be replaced by something like objects_by_type?
            like objects={ projectiles:[], bipeds:[], weapons=[], ... }

    object_iterator_new() types: (also see object_try_and_get_and_verify_type())
        -1 - all
         1 - ? in game_engine_update_purge()
         2 - vehicle
         3 - vehicle seat? unit_seat_filled()
        28 - weapon, item
        32 - projectile
       896 - device group?



      "flag": "0x2006922", player
      "flag": "0x2006900", pistol


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

    # TODO: replace with _object_data_definition+0x08 and _unit_data_definition+0x08
    # TODO: move to separate function computed once at start
    object_datum_size = read_u16(0x1FC0E0)
    unit_datum_size = read_u16(0x1FC188)
    item_datum_size = read_u16(0x1FC380)

    for i in range(object_header_datum_array_total_count):
        # read_u32(object_header_datum_array + 52) + 12 * (weapon_object_handle & 0xFFFF) + 8)
        # tag_address = 32 * read_s16(weapon_object_address) + global_tag_instances_address
        object_address = read_u32(object_header_datum_array_first_element_address + 12 * i + 8)
        if object_address != 0x0:
            tag_name = read_string(read_u32(32 * read_s16(object_address) + global_tag_instances_address + 0x10))
            object_type = read_u8(object_address + 0x64)
            object_type_string = object_string_from_type(object_type)
            objects.append(dict(
                object_id=i,
                address=f'{hex(object_address)} -> {hex(known_addresses[object_address]["host_address"])}',
                header_data=get_formatted_bytes(object_header_datum_array_first_element_address + 12 * i, 12),
                flags=hex(read_u32(object_address + 0x4)),
                x=read_float(object_address + 0xC),
                y=read_float(object_address + 0x10),
                z=read_float(object_address + 0x14),
                vel_x=read_float(object_address + 0x18),
                vel_y=read_float(object_address + 0x1C),
                vel_z=read_float(object_address + 0x20),
                ang_vel_x=read_float(object_address + 0x3C),
                ang_vel_y=read_float(object_address + 0x40),
                ang_vel_z=read_float(object_address + 0x44),
                time_existing=read_s16(object_address + 0x6C),
                unk_damage_1=read_s16(object_address + 0x68),
                owner_unit_ref=hex(read_u32(object_address + 0x70)),  # from projectile_collision() damage section
                owner_object_ref=hex(read_u32(object_address + 0x74)),  # from projectile_collision() damage section
                parent_ref=hex(read_u32(object_address + 0xCC)),  # for held weapons, changes to 0xffffffff when in backpack
                # owner=read_u32(object_address + 0x1E0),
                # owner_hex=hex(read_u32(object_address + 0x1E0)),
                ultimate_parent=hex(read_u32(object_address + 0x1E4)),
                state_flags=read_u8(object_address + 0x1A4),  # 3 if held in inventory, 4 if dropping/moving, 8 if stationary on floor
                drop_time=read_u32(object_address + 0x1B4),  # object gets deleted in game_engine_update_purge() after 30 seconds
                object_type=object_type,
                object_type_string=object_type_string,
                tag_name=tag_name,
            ))
            if object_type_string == 'projectile':
                # we really want the sizeof(item_datum), since that includes individual object+item sizes)
                projectile_address = object_address + item_datum_size
                objects[-1].update(dict(
                    # TODO: get_projectile_data()
                    type_specific_data=dict(
                        flags=read_u32(projectile_address),
                        address=f'{hex(projectile_address)} -> {hex(known_addresses[projectile_address]["host_address"])}',
                        action=read_s16(projectile_address + 0x4),
                        hit_material_type=read_s16(projectile_address + 0x6),
                        ignore_object_index=read_s32(projectile_address + 0x8),
                        target_object_index=read_s32(projectile_address + 0x1C),
                        detonation_timer=read_float(projectile_address + 0x14),
                        detonation_timer_delta=read_float(projectile_address + 0x18),
                        arming_time=read_float(projectile_address + 0x1C),
                        arming_time_delta=read_float(projectile_address + 0x20),
                        distance_traveled=read_float(projectile_address + 0x24),
                        deceleration_timer=read_float(projectile_address + 0x28),
                        deceleration_timer_delta=read_float(projectile_address + 0x2C),
                        deceleration=read_float(projectile_address + 0x30),
                        maximum_damage_distance=read_float(projectile_address + 0x34),
                        rotation_axis_x=read_float(projectile_address + 0x3C),
                        rotation_axis_y=read_float(projectile_address + 0x40),
                        rotation_axis_z=read_float(projectile_address + 0x44),
                        rotation_sine=read_float(projectile_address + 0x48),
                        rotation_cosine=read_float(projectile_address + 0x4C),
                    )
                ))
                # pprint(objects[-1]['type_specific_data'])

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


def get_fog():

    fog_params_address = 0x2FC8A8

    fog_params = dict(
        fog_params_address=f'{hex(0x2FC8A8)} -> {hex(get_host_address(0x2FC8A8))}',
        fog_color_r=read_float(fog_params_address + 0x4),
        fog_color_g=read_float(fog_params_address + 0x8),
        fog_color_b=read_float(fog_params_address + 0xC),
        fog_max_density=read_float(fog_params_address + 0x10),
        fog_atmo_min_dist=read_float(fog_params_address + 0x14),  # defaults to 1024?
        fog_atmo_max_dist=read_float(fog_params_address + 0x18),  # defaults to 2048?
    )

    return fog_params


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


def get_player_ui_globals(local_player):

    if local_player == -1:
        return {}
    
    player_ui_globals_address = 0x2E40D0
    return dict(
        address=hex(get_host_address(player_ui_globals_address + local_player * 56)),
        # TODO: profile name is at +0 widechar
        color=read_u8(player_ui_globals_address + local_player * 56 + 24),
        button_config=read_u8(player_ui_globals_address + local_player * 56 + 40),
        joystick_config=read_u8(player_ui_globals_address + local_player * 56 + 41),
        sensitivity=read_u8(player_ui_globals_address + local_player * 56 + 42),
        joystick_inverted=read_u8(player_ui_globals_address + local_player * 56 + 43),
        rumble_enabled=read_u8(player_ui_globals_address + local_player * 56 + 44),
        flight_inverted=read_u8(player_ui_globals_address + local_player * 56 + 45),
        autocenter_enabled=read_u8(player_ui_globals_address + local_player * 56 + 46),
        active_player_profile_index=f'{hex(read_u32(player_ui_globals_address + local_player * 56 + 48))}',  # used for saving profile data
        joined_multiplayer_game=read_u8(player_ui_globals_address + local_player * 56 + 52),
    )


def get_input_data(local_player_index, player_id):
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

    # if local_player_index == -1:
    #     return {}

    # FIXME: this doesn't seem to use local_player_index -- controller ports 2 and 3 are not input[1] and input[2]
    player_control_address = read_u32(0x276794)
    # player_id = local_player_index

    update_client_player_address = read_u32(read_u32(0x2E8870) + 0x34)
    button_field = read_u8(update_client_player_address + 0x28 * player_id + 0x4)
    action_field = read_u8(update_client_player_address + 0x28 * player_id + 0x5)

    return dict(
        local_player_index=local_player_index,
        # player_look_yaw_rates=read_bytes(0x2E4684, 16),
        # player_look_pitch_rates=read_bytes(0x2E4694, 16),
        look_yaw_rate=read_float(0x2E4684 + 4 * local_player_index),  # these get set to the values from input_abstraction_globals
        look_pitch_rate=read_float(0x2E4694 + 4 * local_player_index),
        input_abstraction_globals=f'{hex(read_u32(0x2E45A0))} @ {hex(get_host_address(0x2E45A0))}',  # are these indexes into the raw controller input blob?
        player_control_pointer=f'{hex(read_u32(0x276794))} @ {hex(get_host_address(0x276794))}',  # see get_local_player_input_blob() and player_control_test_action*()
        player_control=f'{hex(read_u32(read_u32(0x276794)))} @ {hex(get_host_address(read_u32(0x276794)))}',
        player_control_state=dict(
            player_desired_yaw=read_float((local_player_index << 6) + player_control_address + 0x1C),
            # looks like rotation angle in radians, between 0 and 2pi, see sub_B6EA0 in 2276betaP
            player_desired_pitch=read_float((local_player_index << 6) + player_control_address + 0x20),
            # angle in radians between -pi/2 (down) and pi/2 (up)
            player_zoom_level=read_s16((local_player_index << 6) + player_control_address + 16 + 0x24),
            # TODO: remove +16
            player_aim_assist_target=hex(read_u32((local_player_index << 6) + player_control_address + 16 + 0x28)),
            player_aim_assist_near=read_float((local_player_index << 6) + player_control_address + 16 + 0x2C),
            player_aim_assist_far=read_float((local_player_index << 6) + player_control_address + 16 + 0x30),
        ) if local_player_index != -1 else {},

        # input_abstraction is after button/joystick layouts are applied (e.g. actions, not buttons)
        input_abstraction_input_state=dict(
            # 28 * local player index
            address=f'{hex(get_host_address(0x2E4600))}',
            # button values are the length of time the button has been held, in ticks, up to 255
            a=read_u8(0x2E4600 + 0x1C * local_player_index + 0x0),
            black=read_u8(0x2E4600 + 0x1C * local_player_index + 0x1),
            x=read_u8(0x2E4600 + 0x1C * local_player_index + 0x2),
            y=read_u8(0x2E4600 + 0x1C * local_player_index + 0x3),
            b=read_u8(0x2E4600 + 0x1C * local_player_index + 0x4),
            white=read_u8(0x2E4600 + 0x1C * local_player_index + 0x5),
            left_trigger=read_u8(0x2E4600 + 0x1C * local_player_index + 0x6),
            right_trigger=read_u8(0x2E4600 + 0x1C * local_player_index + 0x7),
            start=read_u8(0x2E4600 + 0x1C * local_player_index + 0x8),
            back=read_u8(0x2E4600 + 0x1C * local_player_index + 0x9),
            left_stick_button=read_u8(0x2E4600 + 0x1C * local_player_index + 0xA),
            right_stick_button=read_u8(0x2E4600 + 0x1C * local_player_index + 0xB),
            left_stick_vertical=read_float(0x2E4600 + 0x1C * local_player_index + 0xC),  # up = 1.0, down = -1.0
            left_stick_horizontal=read_float(0x2E4600 + 0x1C * local_player_index + 0x10),  # left = 1.0, right = -1.0
            right_stick_horizontal=read_float(0x2E4600 + 0x1C * local_player_index + 0x14),  # left = 1.0, right = -1.0
            right_stick_vertical=read_float(0x2E4600 + 0x1C * local_player_index + 0x18),  # up = 1.0, down = -1.0
        ) if local_player_index != -1 else {},

        # input_gamepad_state is raw controller values, regardless of button layout preferences
        # input_gamepad_state also includes local controllers that are not playing in the game
        input_gamepad_state=dict(
            address=f'{hex(get_host_address(0x276AFC + 0x28 * local_player_index))}',
            address2=f'{hex(get_host_address(0x276A5C + 0x28 * local_player_index))}',
            a=read_u8(0x276A5C + 0x28 * local_player_index + 0x0),
            b=read_u8(0x276A5C + 0x28 * local_player_index + 0x1),
            x=read_u8(0x276A5C + 0x28 * local_player_index + 0x2),
            y=read_u8(0x276A5C + 0x28 * local_player_index + 0x3),
            black=read_u8(0x276A5C + 0x28 * local_player_index + 0x4),
            white=read_u8(0x276A5C + 0x28 * local_player_index + 0x5),
            left_trigger=read_u8(0x276A5C + 0x28 * local_player_index + 0x6),  # 0 to 255, amount held down
            right_trigger=read_u8(0x276A5C + 0x28 * local_player_index + 0x7),
            # NOTE: 0x8 through 0xF seem to be 0x00 until a button is pressed for the first time, becoming 0xDF while pressed, then stay 0x40
            a_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x10),
            b_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x11),
            x_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x12),
            y_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x13),
            black_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x14),
            white_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x15),
            left_trigger_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x16),
            right_trigger_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x17),
            dpad_up_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x18),
            dpad_down_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x19),
            dpad_left_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x1A),
            dpad_right_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x1B),
            left_stick_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x1E),
            right_stick_duration=read_u8(0x276A5C + 0x28 * local_player_index + 0x1F),
            left_stick_horizontal=read_s16(0x276A5C + 0x28 * local_player_index + 0x20),  # these are signed ints from -32768 to 32769
            left_stick_vertical=read_s16(0x276A5C + 0x28 * local_player_index + 0x22),
            right_stick_horizontal=read_s16(0x276A5C + 0x28 * local_player_index + 0x24),
            right_stick_vertical=read_s16(0x276A5C + 0x28 * local_player_index + 0x26),
        ) if local_player_index != -1 else {},

        # actions compressed for network.
        # this is the only data we have for nonlocal players
        # TODO: find out if this gets also translated to an equivalent of input_abstraction for nonlocal players (e.g. duration held)
        update_queue_values=dict(
            address=f'{hex(get_host_address(update_client_player_address + 0x28 * player_id))}',
            unit_ref=f'{hex(read_u16(update_client_player_address + 0x28 * player_id))}',
            button_field=f'{hex(button_field)}',
            button_crouch=button_field & 0x1,
            button_jump=button_field & 0x2,
            button_fire=button_field & 0x8,
            button_flashlight=button_field & 0x10,
            button_reload=button_field & 0x40,
            button_melee=button_field & 0x80,
            action_field=f'{hex(action_field)}',
            button_throw_grenade=action_field & 0x30,
            button_action=action_field & 0x40,
            desired_yaw=read_float(update_client_player_address + 0x28 * player_id + 0xC),
            desired_pitch=read_float(update_client_player_address + 0x28 * player_id + 0x10),
            forward=read_float(update_client_player_address + 0x28 * player_id + 0x14),
            left=read_float(update_client_player_address + 0x28 * player_id + 0x18),
            right_trigger_held=read_float(update_client_player_address + 0x28 * player_id + 0x1C),  # counts up from 0.0 to 1.0 over 10 (?) seconds
            desired_weapon=read_u16(update_client_player_address + 0x28 * player_id + 0x20),
            desired_grenades=read_u16(update_client_player_address + 0x28 * player_id + 0x22),
            zoom_level=read_s16(update_client_player_address + 0x28 * player_id + 0x24),
        ),
        player_ui_globals=get_player_ui_globals(local_player_index)  # TODO: only need to get this once at start of each game
    )


def get_first_person_weapon(local_player_index):
    """
    Weapon states:
        0   idle
        5   idle animation
        6   firing
        10  meleeing
        14  reloading
        19  readying (switching)
        20  grenading
    :param local_player_index:
    :return:
    """

    weapon_address = read_u32(0x276B48) + 7840 * local_player_index

    return dict(
        address=f'{weapon_address:#x} -> {get_host_address(weapon_address):#x}',
        weapon_rendered=read_u32(weapon_address),  # TODO: confirm if this is actually weapon_rendered
        player_object=f'{read_u32(weapon_address + 4):#x}',  # player object id?
        weapon_object=f'{read_u32(weapon_address + 8):#x}',  # weapon object id?
        state=read_s16(weapon_address + 12),
        idle_animation_threshold=read_s16(weapon_address + 14),
        idle_animation_counter=read_s16(weapon_address + 16),
        animation_id=read_s16(weapon_address + 22),  # TODO: not sure if this is animation id or something else
        animation_tick=read_s16(weapon_address + 24),
        # word_26=read_s16(weapon_address + 26),
        # word_28=read_s16(weapon_address + 28),
        # word_32=read_s16(weapon_address + 32),
        # dword_36=read_s32(weapon_address + 36),
        # f_40=read_float(weapon_address + 40),
        # f_48=read_float(weapon_address + 48),
        # f_52=read_float(weapon_address + 52),
        # f_56=read_float(weapon_address + 56),
        # f_60=read_float(weapon_address + 60),
        # f_64=read_float(weapon_address + 64),
        # f_68=read_float(weapon_address + 68),
        # f_96=read_float(weapon_address + 96),
        # f_100=read_float(weapon_address + 100),
        # f_104=read_float(weapon_address + 104),
        # f_108=read_float(weapon_address + 108),
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


def get_model_nodes(base_address):

    model_node_offsets = [
        # 0x438,  # player location
        0x4a8,
        0x4dc,
        0x510,
        0x544,
        0x578,
        0x5ac,
        0x5e0,
        0x614,
        0x648,
        0x67c,
        0x6b0,
        0x6e4,
        0x718,
        0x74c,
        0x780,
        0x7b4,
        0x7e8,
        0x81c,
        0x850,
        # 0xd84,  # primary weapon
        # 0xfd8,  # primary weapon
        # 0x100c,  # primary weapon
        # 0x1040,  # primary weapon
        # 0x1074,  # primary weapon
        # 0x10a8,  # primary weapon
        # 0x10dc,  # primary weapon
        # 0x1110,  # primary weapon
    ]

    model_nodes = []

    for offset in model_node_offsets:
        model_nodes.append((
            read_float(base_address + offset),
            read_float(base_address + offset + 4),
            read_float(base_address + offset + 8),
        ))

    return model_nodes


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


def get_all_team_scores():

    return dict(
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


def get_global_variant():

    global_variant_address = 0x2F90A8


def get_game_variant_global():

    game_variant_global_address = 0x2FAB60
    return dict(
        address=hex(get_host_address(game_variant_global_address)),
        values=get_formatted_bytes(game_variant_global_address, 0x68),
    )


def player_score_by_player_id(player_id, gametype):

    # ctf player scores are stored in static player object
    if gametype == 1:
        return 0

    return read_s32(player_score_addresses_by_gametype[gametype] + 4 * player_id)


def team_score_by_team_id(team_id, gametype):

    return read_s32(team_score_addresses_by_gametype[gametype] + 4 * team_id)


def get_network_game_data(network_game_data_address):

    machine_count = read_s16(network_game_data_address + 274)
    network_machines_address = network_game_data_address + 276
    player_count = read_s16(network_game_data_address + 548)  # from network_game_add_player
    network_players_address = network_game_data_address + 550  # from netgame_unjoin_player

    return dict(
        player_count=player_count,
        maximum_player_count=read_u8(network_game_data_address + 270),
        machine_count=machine_count,
        network_machines=[dict(
            name=read_wchar(network_machines_address + 68 * i),
            machine_index=read_u8(network_machines_address + 68 * i + 64),
        ) for i in range(machine_count)],
        network_players=[dict(
            name=read_wchar(network_players_address + 32 * i, 24),
            color=read_s16(network_players_address + 32 * i + 24),
            unused=read_s16(network_players_address + 32 * i + 26),
            machine_index=read_u8(network_players_address + 32 * i + 28),
            controller_index=read_u8(network_players_address + 32 * i + 29),
            team=read_u8(network_players_address + 32 * i + 30),
            player_list_index=read_u8(network_players_address + 32 * i + 31),
        ) for i in range(player_count)]
    )


def get_network_game_client():

    network_game_client_address = 0x2FB180

    return dict(
        machine_index=read_u16(network_game_client_address),
        advertised_games=dict(),  # 9 games
        ping_target_ip=hex(read_s32(network_game_client_address + 2056)),
        packets_sent=read_s16(network_game_client_address + 2084),
        packets_received=read_s16(network_game_client_address + 2086),
        average_ping=read_s16(network_game_client_address + 2088),
        ping_active=read_u8(network_game_client_address + 2090),
        seconds_to_game_start=read_s16(network_game_client_address + 3236),

        # TODO: this should be dynamic depending on whether we're a client or server
        network_game_data=get_network_game_data(network_game_client_address + 2140)  # from network_game_client_add_player_to_game
    )


def get_network_game_server():

    # from network_game_server_create(), network_game_server_memory_do_not_use_directly
    network_game_server_address = 0x2FBE40

    return dict(
        address=hex(get_host_address(network_game_server_address)),
        # values=get_formatted_bytes(network_game_server_address, 1212),
        countdown_active=read_u8(network_game_server_address + 1172),
        countdown_paused=read_u8(network_game_server_address + 1173),
        countdown_adjusted_time=read_u8(network_game_server_address + 1174),
    )


def dump_game_update_contents():
    # FIXME: this whole section is only valid if you're breaking in network_game_client_handle_game_update()

    network_game_client = 0x2FB180
    data_queue = 0x2E87E4
    packet_data_address = 0xD00E82D0  # TODO: find this dynamically -- this was pulled directly from IDA/gdb and changes on restart

    # FIXME: workaround for caching network data, need real size (520 max?)
    get_host_address(packet_data_address)
    add_to_cache(packet_data_address, 5000)
    get_host_address(data_queue)
    add_to_cache(data_queue, 520)

    update_queue_address = read_u32(0x2E8870)
    update_client_player_address = read_u32(read_u32(0x2E8870) + 0x34)
    update_client_blind_first_element_address = read_u32(0x2E8870) + 0x38
    if update_client_player_address != update_client_blind_first_element_address:
        print(f'==> WARNING: update client queue address mismatch: {update_client_player_address} != {update_client_blind_first_element_address}')

    return dict(
        packet_data_address=f'{packet_data_address:#x} -> {get_host_address(packet_data_address):#x}',
        network_game_client=f'{network_game_client:#x} -> {get_host_address(network_game_client):#x}',
        data_queue_address=f'{data_queue:#x} -> {get_host_address(data_queue):#x}',
        dword_2E87E4=f'{hex(read_u32(0x2E87E4))} & 0x7F = {read_u32(0x2E87E4) & 0x74}',
        dword_2E87E8=hex(read_u32(0x2E87E8)),
        dword_2E8870=f'{hex(read_u32(0x2E8870))} -> {hex(get_host_address(read_u32(0x2E8870)))}',
        dword_2E8874=hex(read_u32(0x2E8874)),
        dword_2E8870_plus46=hex(read_s16(read_u32(0x2E8870) + 46)),
        dword_2E8870_plus52=f'{hex(read_u32(read_u32(0x2E8870) + 52))} -> {hex(get_host_address(read_u32(read_u32(0x2E8870) + 52)))}',
        header=dict(
            tick=read_s32(data_queue),
            global_random=hex(read_u32(data_queue + 4)),
            tick_2=read_s32(data_queue + 8),
            unk_1=read_u16(data_queue + 12),  # player index?
            player_count=read_s16(data_queue + 14),
        ),
        data=dict(
            # desired_yaw=read_float(data_queue + 52),
            # desired_pitch=read_float(data_queue + 56),
            # unk_float_1=read_float(data_queue + 288),
            # observer_aim_x_neg=read_float(data_queue + 292),  # NOTE: sign is swapped on these
            # observer_aim_y_neg=read_float(data_queue + 296),
            # observer_aim_z_neg=read_float(data_queue + 300),
            # unk_float_2=read_float(data_queue + 304),
            # observer_aim_x=read_float(data_queue + 308),
            # observer_aim_y=read_float(data_queue + 312),
            # observer_aim_z=read_float(data_queue + 316),
            # observer_camera_x=read_float(data_queue + 380),
            # observer_camera_y=read_float(data_queue + 384),
            # observer_camera_z=read_float(data_queue + 388),
        ),
        # raw_data=get_formatted_bytes(data_queue + 16, 520),
        update_queue_header=get_formatted_bytes(read_u32(0x2E8870), 0x34),
        update_queue_values=dict(
            unk_1=read_s16(update_queue_address + 0x20),  # max element count?
            unk_2=read_s16(update_queue_address + 0x22),  # element length?
            unk_3=read_s16(update_queue_address + 0x24),  # not sure
            unk_4=read_s16(update_queue_address + 0x2E),  # element count?
            unk_5=read_s16(update_queue_address + 0x30),  # also element count?
            unk_6=f'{hex(read_u16(update_queue_address + 0x32))} ({read_u16(update_queue_address + 0x32)})',
        ),
        queue_ids=' '.join([hex(read_u16(update_client_blind_first_element_address + 0x28 * i))[2:] for i in range(20)]),
    )


def get_animation_debug_info(unk_handle, animation_id, animation_tick):
    """
    from animation_update_internal()

    :param unk_handle:
    :param animation_id:
    :param animation_tick:
    :return:
    """

    tag_address = read_u32(32 * (unk_handle & 0xFFFF) + global_tag_instances_address + 20)
    animation_address = read_u32(tag_address + 120) + 180 * animation_id

    animation_length = read_s16(animation_address + 34)
    unk_46 = read_s16(animation_address + 46)
    unk_52 = read_s16(animation_address + 52)
    unk_54 = read_s16(animation_address + 54)

    if animation_tick < animation_length:
        if animation_tick != animation_length or unk_46 != 0:
            result = int(animation_tick + 1 == unk_52 or animation_tick == unk_54)
        else:
            result = 2
    else:
        if unk_46 <= 0:
            result = 3
        else:
            result = 4

    return dict(
        tag_address=hex(tag_address),
        animation_address=hex(animation_address),
        animation_length=animation_length,
        unk_handle=hex(unk_handle),
        animation_id=animation_id,
        animation_tick=animation_tick,
        unk_46=unk_46,
        unk_52=unk_52,
        unk_54=unk_54,
        result=result
    )


def arrange_objects_by_type(objects):
    """
    :param objects:
    :return:
    """

    objects_meta = dict(
        object_indexes_by_type=defaultdict(list),
        object_ids_by_type=defaultdict(list),
        projectiles_by_unit_id=defaultdict(list)
    )

    for i, o in enumerate(objects):
        object_type = o['object_type_string']
        objects_meta['object_indexes_by_type'][object_type].append(i)
        objects_meta['object_ids_by_type'][object_type].append(o['object_id'])
        if object_type == 'projectile':
            # player_id = int(o['ultimate_parent'], 16) & 0xFFFF
            objects_meta['projectiles_by_unit_id'][o['owner_unit_ref']].append(i)

    # if objects_by_type['projectiles_by_unit_id']:
    #     print(objects_by_type['projectiles_by_unit_id'])

    return objects_meta


def get_map_info():

    map_header_address = 0x2DFC98 - 4820

    return dict(
        address=hex(get_host_address(map_header_address)),
        cache_version=read_u32(map_header_address + 0x4),
        file_size=read_u32(map_header_address + 0x8),
        padding_length=read_u32(map_header_address + 0xC),
        tag_data_offset=read_u32(map_header_address + 0x10),
        tag_data_size=read_u32(map_header_address + 0x14),
        scenario_name=read_string(map_header_address + 0x20, 32),
        build_version=read_string(map_header_address + 0x40, 32),
        scenario_type=read_u16(map_header_address + 0x60),
        checksum=read_u32(map_header_address + 0x64),
        # bytes=get_formatted_bytes(map_header_address, 2048)
    )


def get_game_info():

    # FIXME: also support campaign (e.g. prisoner bots)
    #        currently fails when getting gametype for score

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

    # TODO: also check if this is a multiplayer game or campaign
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

            previous_dynamic_player_address = read_u32(object_header_datum_array_first_element_address + (
                        previous_player_object_handle & 0xFFFF) * object_header_datum_array_element_size + 8)

            # print('dynamic player address: {} | {}'.format(hex(dynamic_player_address), dynamic_player_address))
            # print('player_object_handle: {} | {}'.format(hex(player_object_handle), player_object_handle))

            player_object_debug = dict(
                player_object_handle=hex(player_object_handle),
                # player_object_handle_u32=hex(read_u32(static_player_address + 0x34)),
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
            # FIXME: if saving full game replay takes too long, this will return 0x0 + 0x3E0
            if player_object_handle == -1:
                damage_table_address = read_u32(object_header_datum_array_first_element_address + (
                        previous_player_object_handle & 0xFFFF) * object_header_datum_array_element_size + 8) + 0x3E0
            else:
                damage_table_address = dynamic_player_address + 0x3E0
            player_object_debug['damage_table_address'] = f'{hex(damage_table_address)} -> {hex(get_host_address(damage_table_address))}'
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
                        static_player_hex=hex(damage_table[-1]['static_player']),
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
                        can_fire=read_u8(weapon_object_address + 0x259),
                        reload_time=read_s16(weapon_object_address + 0x25A),
                        backpack_ammo_count=read_s16(weapon_object_address + 0x25E),
                        magazine_ammo_count=read_s16(weapon_object_address + 0x260),
                        weapon_tag_address=f'{read_u32(tag_address)} @ {hex(tag_address)} -> {hex(known_addresses[tag_address]["host_address"])}',
                        # owner=read_u32(weapon_object_address + 0x1E0),  # TODO: this isn't really owner, seems to correlate to current action
                        # owner_hex=hex(read_u32(weapon_object_address + 0x1E0)),
                        energy_used=read_float(weapon_object_address + 0x1F0),  # used for whether to delete dropped energy weapon (if == 1.0)
                        weapon_type=weapon_type,  # from weapon_trigger_fire()
                        is_energy_weapon=is_energy_weapon,
                        zoom_levels=read_s16(read_u32(tag_address + 20) + 986),
                        zoom_min=read_float(read_u32(tag_address + 20) + 988),
                        zoom_max=read_float(read_u32(tag_address + 20) + 992),
                        autoaim_angle=read_float(read_u32(tag_address + 20) + 996),  # radians, from unit_get_aim_assist_parameters()
                        autoaim_range=read_float(read_u32(tag_address + 20) + 1000),
                        magnetism_angle=read_float(read_u32(tag_address + 20) + 1004),
                        magnetism_range=read_float(read_u32(tag_address + 20) + 1008),
                        deviation_angle=read_float(read_u32(tag_address + 20) + 1012),
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
                    flags=read_u32(dynamic_player_address + 0x4),  # & 0x10000 is garbage_bit, & 8 is connected_to_map_bit, & 1 is 1 for vehicle weapons (checked in find_aim_assist_targets_recursive())
                    x=read_float(dynamic_player_address + 0xC),
                    y=read_float(dynamic_player_address + 0x10),
                    z=read_float(dynamic_player_address + 0x14),
                    x_vel=read_float(dynamic_player_address + 0x18),  # object.translational_velocity
                    y_vel=read_float(dynamic_player_address + 0x1C),
                    z_vel=read_float(dynamic_player_address + 0x20),
                    legs_pitch=read_float(dynamic_player_address + 0x24),  # legs? TODO: see end of sub_152E40() in 2276betaP, looks like object.forward and object.up for next 6 floats
                    legs_yaw=read_float(dynamic_player_address + 0x28),  # legs?
                    legs_roll=read_float(dynamic_player_address + 0x2C),  # legs?
                    pitch1=read_float(dynamic_player_address + 0x30),  # these get set in biped_snap_facing(), not sure what it is. (0, 0, 1) in most cases
                    yaw1=read_float(dynamic_player_address + 0x34),
                    roll1=read_float(dynamic_player_address + 0x38),
                    ang_vel_x=read_float(dynamic_player_address + 0x3C),
                    ang_vel_y=read_float(dynamic_player_address + 0x40),
                    ang_vel_z=read_float(dynamic_player_address + 0x44),
                    aim_assist_sphere_x=read_float(dynamic_player_address + 0x50),  # center point? used in find_aim_assist_targets_recursive()
                    aim_assist_sphere_y=read_float(dynamic_player_address + 0x54),
                    aim_assist_sphere_z=read_float(dynamic_player_address + 0x58),
                    aim_assist_sphere_radius=read_float(dynamic_player_address + 0x5C),  # sphere radius? find_aim_assist_targets_recursive()
                    scale=read_float(dynamic_player_address + 0x60),  # object.scale (items only?)
                    type=read_u16(dynamic_player_address + 0x64),
                    render_flags=read_u16(dynamic_player_address + 0x66),
                    weapon_owner_team=read_s16(dynamic_player_address + 0x68),  # weapon.owner_team_index (e.g. ctf) -- also used in find_aim_assist_targets_recursive() for team check
                    powerup_unk2=read_s16(dynamic_player_address + 0x6A),
                    idle_ticks=read_s16(dynamic_player_address + 0x6C),
                    # animation_unk_1=hex(read_u32(dynamic_player_address + 0x7C)),
                    # animation_unk_2=hex(read_s16(dynamic_player_address + 0x80)),
                    # animation_unk_3=hex(read_s16(dynamic_player_address + 0x82)),
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
                    shields_status=read_u16(dynamic_player_address + 0xB6),  # 0x0 normally, 0x10 while overshield charging, 0x1000 while shields charging, 0x8 while shields are fully depleted
                    shields_status_hex=hex(read_u16(dynamic_player_address + 0xB6)),

                    next_object=read_s32(dynamic_player_address + 0xC4),
                    next_object_2=hex(read_u32(dynamic_player_address + 0xC8)),  # used in find_aim_assist_targets_recursive(), seems to be object handle for next object in object table
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

                                                                                # from discord, ControlFlags:
                                                                                #   crouch = 0x1
                                                                                #   jump = 0x2
                                                                                #   UserAnimation1 = 0x4
                                                                                #   UserAnimation2 = 0x8
                                                                                #   IntegratedLight = 0x10
                                                                                #   ExactFacing = 0x29
                                                                                #   Action = 0x40
                                                                                #   UseEquipment = 0x80
                                                                                #   LookDontTurn = 0x100
                                                                                #   ForceAlert = 0x200
                                                                                #   Reload = 0x400
                                                                                #   PrimaryTrigger = 0x800
                                                                                #   SecondaryTrigger = 0x1000
                                                                                #   ThrowGrenade = 0x2000
                                                                                #   SwapWeapons = 0x4000
                    # stunned=read_s32(dynamic_player_address + 0x1CB),  # from biped_jump -- this isn't actually stunned
                    stunned=read_float(dynamic_player_address + 0x3D4),  # from biped_jump -- this isn't actually stunned
                    # maybe_desired_facing_vector_x=read_float(dynamic_player_address + 0x1C8),
                    # maybe_desired_facing_vector_y=read_float(dynamic_player_address + 0x1CC),  # FIXME: y is null
                    # maybe_desired_facing_vector_z=read_float(dynamic_player_address + 0x1D0),
                    xunk0=read_float(dynamic_player_address + 0x1D4),  # unknown, from biped_update_turning(), gets multiplied by leg rotation 24, 28, 2c.
                    yunk0=read_float(dynamic_player_address + 0x1D8),
                    zunk0=read_float(dynamic_player_address + 0x1DC),  # z seems to stay at 0.0, but periodically will briefly flip to same z as others
                    xaima=read_float(dynamic_player_address + 0x1E0),  # unit vectors, -1 to 1 on x y z axes.
                    yaima=read_float(dynamic_player_address + 0x1E4),
                    zaima=read_float(dynamic_player_address + 0x1E8),
                    aiming_vector_x=read_float(dynamic_player_address + 0x1EC),  # used in first_person_camera_deterministic(), which gets used in player_aim_projectile()
                    aiming_vector_y=read_float(dynamic_player_address + 0x1F0),
                    aiming_vector_z=read_float(dynamic_player_address + 0x1F4),
                    xaim0=read_float(dynamic_player_address + 0x1F8),  # these seem to be used for projectiles -- see projectile_update()
                    yaim0=read_float(dynamic_player_address + 0x1FC),
                    zaim0=read_float(dynamic_player_address + 0x200),
                    xaim1=read_float(dynamic_player_address + 0x204),  # look in players_update_before_game() and unit_control()
                    yaim1=read_float(dynamic_player_address + 0x208),
                    zaim1=read_float(dynamic_player_address + 0x20C),
                    looking_vector_x=read_float(dynamic_player_address + 0x210),
                    looking_vector_y=read_float(dynamic_player_address + 0x214),
                    looking_vector_z=read_float(dynamic_player_address + 0x218),
                    move_forward=read_float(dynamic_player_address + 0x228),  # throttle?
                    move_left=read_float(dynamic_player_address + 0x22C),
                    move_up=read_float(dynamic_player_address + 0x230),  # not sure if this is used anywhere? banshee controls? observer?

                    # note: check out search for header->event_type in 2276betaP, animation types? (not sure if these are the same animations, but noting here anyway for later)
                    #       & 0xFC == 8     _playback_animation_state_set
                    #       & 0xFC == 12    _playback_aiming_speed_set
                    #       & 0xFC == 16    _playback_control_flags_set
                    #       & 0xFC == 20    _playback_weapon_index_set
                    #       & 0xFC == 24    _playback_throttle_set
                    melee_damage_type=read_u8(dynamic_player_address + 0x239),  # see unit_cause_continuous_melee_damage(), if =4 then continuous melee damage, if =3 then impact melee damage, players are =0
                    animation_1=read_u8(dynamic_player_address + 0x253),  # see unit_update_animation() and unit_get_custom_animation_time(), 0x253 and 0x254 both seem related to animations (movement, grenade throwing, melee, etc)
                    animation_2=read_u8(dynamic_player_address + 0x254),
                    animation_debug=get_animation_debug_info(read_u32(dynamic_player_address + 0x7C), read_s16(dynamic_player_address + 0x80), read_s16(dynamic_player_address + 0x82)),
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
                    current_equipment=hex(read_u32(dynamic_player_address + 0x2C8)),
                    primary_nades=read_u8(dynamic_player_address + 0x2CE),
                    secondary_nades=read_u8(dynamic_player_address + 0x2CF),
                    zoom_level=read_s8(dynamic_player_address + 0x2D0),

                    camo_amount=read_float(dynamic_player_address + 0x32C),  # 0=nocamo, 1=fullcamo, from game_engine_player_depower_active_camo(), also see unit_update()
                    # camo_thing2=read_float(dynamic_player_address + 0x330),  # from first_person_weapon_draw() and unit_update()

                    # 0 normally, 1 when player has camo and is revealed by shooting (but not being shot at)
                    camo_self_revealed=read_u16(dynamic_player_address + 0x3D2),  # from player_powerup_on(), not sure when this actually gets set

                    # see game_statistics_record_kill() and unit_record_damage()
                    damagers_list_address=hex(get_host_address(dynamic_player_address + 0x3E0)),
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

                    # TODO: need to verify padding on these. crouchscale doesn't line up with the end of `short landing`
                    slipping_ticks=read_u8(dynamic_player_address + 0x45A),
                    stop_ticks=read_u8(dynamic_player_address + 0x45B),
                    jump_recovery_timer=read_u8(dynamic_player_address + 0x45C),
                    melee_animation_remaining=read_u8(dynamic_player_address + 0x45D),
                    melee_animation_damage_tick=read_u8(dynamic_player_address + 0x45E),  # from biped_update() and unit_cause_player_melee_damage()
                    melee_impact_this_tick=read_u8(dynamic_player_address + 0x45D) == read_u8(dynamic_player_address + 0x45E),  # TODO: move to computed?
                    landing=read_u16(dynamic_player_address + 0x45F),

                    air_3_0x460=read_s16(dynamic_player_address + 0x460),  # biped_update(), if -1 check for slipping. stays -1 while walking, briefly 0 when landing, 1 if damaged from fall? stays at 0 or 1 until 0x428 reaches 0x429

                    # 0x4096 when shields are charging, 0x4112 when overshield charging
                    air_4_0xB6=read_s16(dynamic_player_address + 0xB6),  # biped_flying_through_air() and unit_get_camera_position(), 8 while shields are damaged from falling or nade, 4096 while shields recharging (from any damage)

                    biped_flags=read_u32(biped_tag_address + 0x2F4),
                    autoaim_pill_radius=read_float(biped_tag_address + 0x458),  # from biped_get_autoaim_pill()
                )

                model_nodes = get_model_nodes(dynamic_player_address)

            else:

                if previous_player_object_handle != -1:
                    # body of dead player
                    model_nodes = get_model_nodes(previous_dynamic_player_address)
                else:
                    model_nodes = []

                player_object_data = {}
                # print('player respawns in {} ticks'.format(read_u32(static_player_address + 0x2C)))

            # print(player_object_data['xaim2'], player_object_data['yaim2'], player_object_data['zaim2'])

            # TODO: game_engine_get_state_message()

            local_player = read_s16(static_player_address + 0x2)

            player_stats = dict(
                player_index=player_index,  # index in the player datum array
                local_player=local_player,  # 0 to 3 if local (controller port), -1 if not local
                name=read_bytes(static_player_address + 0x4, 24).decode('utf-16').split('\x00', 1)[0] if use_pymem else b''.join([int.to_bytes(i, signed=True) for i in read_bytes(static_player_address + 0x4, 24)]).decode('utf-16').split('\x00', 1)[0],
                # is_dead=hex(read_s32(static_player_address + 0xD)),  # from any_player_is_dead() -- value does not change when dead
                # name=t.read(static_player_address + 0x4, 24).decode('utf-16').split('\x00', 1)[0],
                team=read_u32(static_player_address + 0x20),  # red=0, blue=1, ffa=0-15
                action_target=hex(read_u32(static_player_address + 0x24)),  # looks like the object you'll interact with if you press action, set to -1 on spawn
                action=read_u16(static_player_address + 0x28),  # 6 if standing over weapon (7 if only 1 weapon held), 8 if next to vehicle, 0 otherwise, set to 0 on spawn
                action_seat=read_u16(static_player_address + 0x2A),
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
                shots_fired=read_s32(static_player_address + 0xAE),
                shots_hit=read_s16(static_player_address + 0xB2),
                score=player_score_by_player_id(player_index, read_u32(game_engine_globals_address + 0x4) if game_engine_globals_address else 0),
                ctf_score=read_s16(static_player_address + 0xC4),
                player_quit=read_u8(static_player_address + 0xD1),  # 1 if player quit, not sure what else
                damage_table=damage_table,
                observer_camera_info=get_observer_camera_info(local_player),  # TODO: duplicate lookup
                input_data=get_input_data(local_player, player_index),
                player_object_debug=player_object_debug,
                player_object_data=player_object_data,
                model_nodes=model_nodes,  # also includes dead body while respawning
            )

            derived_stats = dict(
                # has_camo=player_stats['camo_timer'] > 0,
                has_camo=bool(player_object_data) and player_object_data['camo'] == 0x51,
                has_overshield=bool(player_object_data) and (player_object_data['shields_status'] == 0x10 or player_object_data['shields'] > 1),  # FIXME: replace int conversion
            )
            player_stats.update(derived_stats=derived_stats)

            # get data that depends on players being local
            if local_player != -1:
                # player_stats.update(input_data=get_input_data(local_player))
                player_stats.update(first_person_weapon=get_first_person_weapon(local_player))

            player_stat_array.append(player_stats)

    game_info = dict(
        process_id=f'{pid} - {hex(pid)}',
        # pgcr_debug=dict(
        #     arg_0_address=f'{read_u8(0x106536)} @ {0x106536:#x} -> {get_host_address(0x106536):#x}',
        #     arg_1_address=f'{read_u8(0x10653E)} @ {0x10653E:#x} -> {get_host_address(0x10653E):#x}',
        #     maybe_font_size=f'{read_u8(0x10721B + 1)} @ {0x10721B + 1:#x} -> {get_host_address(0x10721B + 1):#x}',
        #     color1=f'{hex(read_u32(0x106F3F + 4))} @ {0x106F3F + 4:#x} -> {get_host_address(0x106F3F + 4):#x}',
        #     color2=f'{hex(read_u32(0x106F51 + 4))} @ {0x106F51 + 4:#x} -> {get_host_address(0x106F51 + 4):#x}',
        #     color3=f'{hex(read_u32(0x106F5D + 4))} @ {0x106F5D + 4:#x} -> {get_host_address(0x106F5D + 4):#x}',
        #     color4=f'{hex(read_u32(0x106F69 + 4))} @ {0x106F69 + 4:#x} -> {get_host_address(0x106F69 + 4):#x}',
        #     color5=f'{hex(read_u32(0x106FFB + 4))} @ {0x106FFB + 4:#x} -> {get_host_address(0x106FFB + 4):#x}',
        #     color6=f'{hex(read_u32(0x106FFB + 12))} @ {0x106FFB + 12:#x} -> {get_host_address(0x106FFB + 12):#x}',
        #     color7=f'{hex(read_u32(0x106FFB + 20))} @ {0x106FFB + 20:#x} -> {get_host_address(0x106FFB + 20):#x}',
        # ),
        game_type=read_u32(game_engine_globals_address + 0x4) if game_engine_globals_address else '',
        variant=read_u8(0x2F90F4),
        global_stage=read_string(0x2FAC20, length=63),  # only populated for hostbox
        multiplayer_map_name=read_string(0x2E37CD),  # populated for host and join boxes
        # network_game_server=f'{hex(read_u32(read_u32(0x2E3628)))}: {hex(read_u32(read_u32(0x2E3628)))} -> {hex(get_host_address(read_u32(read_u32(0x2E3628))))}',
        # network_game_server_state=read_s16(read_u32(0x2E3628) + 0x4),  # 1 = ingame
                                                                       # 2 = postgame
                                                                       # 0 = picking map?
        map_info=get_map_info(),
        game_connection=read_s16(0x2E3684),
        # network_game_client=read_u8(read_u32(0x2E362C)),
        game_engine_has_teams=read_u8(0x2F90C4),
        game_engine_running=game_engine_globals_address != 0,  # true in game and postgame carnage report, false in pregame lobby
        game_engine_can_score=read_u32(0x2FABF0) == 0 and game_engine_globals_address != 0,  # false as soon as you hear "game over"

        # TODO: only look up scores for current gametype
        # input_data=get_input_data(),
        flag_data=get_flag_data(),
        local_player_count=read_u16(players_globals_address + 0x24),
        # key_data=get_key_data(),
        # flag_base_locations=f'{read_float(0x2762A4)} {hex(known_addresses[0x2762A4]["host_address"])}',
        game_time_info=get_game_time_info(),
        # game_variant=get_game_variant_global(),
        network_game_server=get_network_game_server(),
        network_game_client=get_network_game_client(),
        # game_update_data=dump_game_update_contents(),
        # fog_data=get_fog(),
        observer_cameras_address=f'{get_host_address(0x271550):#x}',
        game_globals_address=f'{hex(game_globals_address)} -> {hex(get_host_address(game_globals_address))}',
        game_globals_map_loaded=game_globals_map_loaded,
        players_are_double_speed=read_u8(game_globals_address + 0x2),
        game_loading_in_progress=read_u8(game_globals_address + 0x3),
        precache_map_status=read_float(game_globals_address + 0x4),
        game_difficulty_level=read_u8(game_globals_address + 0xE),
        # FIXME: read_s32(global_game_globals_address + 372) doesn't seem to be valid on first tick?
        #        Error converting gpa 0x3e590bb7 to gva (got {'return': 'Unmapped\r\n'})
        # idle_time_debug_addr=hex(read_s32(global_game_globals_address + 372)),
        # idle_time_debug_addr2=hex(read_s32(global_game_globals_address + 372) + 156),
        # idle_time_lower_bound=read_float(read_s32(global_game_globals_address + 372) + 156),  # in seconds
        # idle_time_upper_bound=read_float(read_s32(global_game_globals_address + 372) + 160),  # in seconds
        # idle_time_skip_fraction=read_float(read_s32(global_game_globals_address + 372) + 164),
        # stun_movement_penalty=read_float(read_s32(global_game_globals_address + 372) + 128),
        # stun_jumping_penalty=read_float(read_s32(global_game_globals_address + 372) + 132),
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

        # TODO: try asyncio or multiprocessing for large blobs like this
        #       (note: tried asyncio.run/await/async and it ran half as fast)
        # TODO: see https://github.com/StarrFox/wizwalker for possible implementation
        #       make the individual pymem calls async?
        objects=get_objects(),
        items=get_items(),
        spawns=get_spawns(),
        game_ended_this_tick=False,  # this gets set in extract_events()
        current_time=datetime.datetime.now(),
    )

    current_time = game_info['current_time']
    elapsed_time = game_info['game_time_info']['game_time'] + 1
    start_time = current_time - datetime.timedelta(seconds=elapsed_time / 30)
    game_info.update(dict(
        start_time=start_time,
        objects_meta=arrange_objects_by_type(game_info['objects'])
    ))

    if ('start_time' not in game_meta or game_meta['start_time'] is None) and game_info['game_engine_can_score']:
        game_meta['start_time'] = start_time

    # FIXME: need a better game id, start_game can shift if the game runs slowly
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
        if 'qmp' in value:
            host = value['host_address']
            memory_map.append([hex(guest), hex(host), guest - last_guest, host - last_host, value['qmp_traceback']])
            if guest - last_guest != host - last_host:
                mismatches.append([hex(guest), hex(host), guest - last_guest, host - last_host, value['qmp_traceback']])
            last_guest = guest
            last_host = host

    print('============= MEMORY MAP =============')
    print('guest, host, guest diff, host diff')
    pprint(memory_map)
    print('============= MISMATCHES =============')
    print('guest, host, guest diff, host diff')
    pprint(mismatches)


# TODO: do something with this
def get_game_data():

    team_game_address = 0x2F90C4
    game_engine_address = 0x2F9110
    game_server_address = 0x2E3628
    game_client_address = 0x2E362C
    game_connection_word = 0x2E3684
    players_globals_address = 0x2FAD20
    team_data_address = 0x2FAD24


    # players_globals_is_dead


def send_to_api(filename):
    pass
    # try:
    #     from replay_manager import handle_new_replay_file
    #     ok = handle_new_replay_file(filename)
    #     if not ok:
    #         print(f"send_to_api: upload failed for {filename}")
    # except Exception as e:
    #     print(f"send_to_api: error while uploading {filename}: {e}")


def send_to_file(data, outfile, compression=''):
    
    # TODO: better serialization of datetime
    # json.dump(data, outfile, default=str)
    os.makedirs(os.path.dirname(outfile), exist_ok=True)
    if compression:
        # TODO: also generate versioned schema
        data_bytes = json.dumps(data, default=str).encode()
        print(f'Saving {len(data_bytes)} bytes to {outfile}')
        if compression == 'gz':
            with gzip.open(outfile, 'wb') as f:
                f.write(data_bytes)
        elif compression == 'lz':
            with lzma.open(outfile, 'wb') as f:
                f.write(data_bytes)
        elif compression == 'br':
            with open(outfile, 'wb') as f:
                f.write(brotli.compress(data_bytes, quality=6))
        # decompress like `zstd -d filename`
        elif compression == 'zstd':
            with open(outfile, 'wb') as f:
                compressor = zstd.ZstdCompressor(level=11)
                # writer = compressor.stream_writer(f)  # stream_writer does work, but seems to not be decompressable via python zstd.decompress()
                # writer.write(data_bytes)
                # writer.flush(zstd.FLUSH_FRAME)
                f.write(compressor.compress(data_bytes))
    else:
        with open(outfile, 'a') as f:
            # f.write(orjson.dumps(data, option=orjson.OPT_INDENT_2 | orjson.OPT_NON_STR_KEYS).decode())
            print(f'Saving uncompressed to {outfile}')
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
write_queue_from_ui = queue.Queue()
api_client_queue = queue.Queue()
ws_client_queue = queue.Queue()


def handle_game_info_loop():
    """
    Continuous loop waiting for new ticks in game_info_queue
    :return:
    """

    # database running locally, use `vagrant up` to start
    # try:
    #     db = DBConnector()
    # except:
    #     print('WARNING: database is not up -- run `vagrant up` to start a local database')

    game_ticks = []
    events = []
    store_all_ticks = True

    # FIXME: try/catch for common or potential exceptions here -- need to keep this thread alive or restart if it dies

    while True:

        # FIXME: PERF: if file output is enabled for every tick, the initial ~100 or so queued ticks
        #              cause a huge slowdown and many missed ticks while the queue is written to disk
        if (s := game_info_queue.qsize()) > 0:
            print(f'queue size: {s}')
            # time.sleep(0.01)

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
            meta = game_info.pop('game_meta', [])
            # objects = game_info.pop('objects', [])  # FIXME: just temporarily removing this for write performance

            # FIXME: PERF: storing game ticks like this uses lots of memory (~1-2MB/s)
            if store_all_ticks:
                game_ticks.append(game_info)
            # FIXME: this doesn't handle game crashes or missing the last tick of the game (or probably some other similar cases)
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
                ) if store_all_ticks else {},
                game_meta=meta,
                events=events,
                spawns=spawns,
                items=items,
                ticks=game_ticks,
            )

            if game_info['game_ended_this_tick']:
                pprint(game['summary'])
                # TODO: do this in a separate process, or async -- need to resume tick capture as soon as we can
                filename = os.path.join(REPLAY_DIRECTORY, f'{game_id}_final.json.zst')
                send_to_file(game, filename, compression=REPLAY_COMPRESSION)
                send_to_api(filename)
                # send_to_file(game, f'{REPLAY_DIRECTORY}/{game_id}_final.json.br', compression='br')
                # send_to_file(game, f'{REPLAY_DIRECTORY}/{game_id}_final.json.gz', compression='gz')
                game_ticks = []

                gc.collect()
            # send_to_file(game_info, f'{REPLAY_DIRECTORY}/{game_id}.jsonl')
            # send_to_file(game_info, f'C:/tmp/replays/{game_id}.jsonl')


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
#     (update 2025: ~6400 pymem reads per tick with new pc, in normal loop, py3.11) (1.73x)
# compared to only 8 qmp calls per tick (250/second)
# note: qmp calls are rate limited by adjusting Test.request_rate_seconds
#       with no rate limiting, speed is 25 qmp calls per tick (750/second)
#
#   go is ~7x as fast, at ~26000 memory reads per tick (when running in GoLand -- xemu-tools-go)
#      (update 2025: ~55000 memory reads per tick with new pc) (2.12x)
# rust is ~9x as fast, at ~34000 memory reads per tick (when running in CLion -- xemu-tools-rust)
#      (update 2025: ~64000 memory reads per tick with new pc) (1.88x)

# thingy = read_float(0x2714D8)  # the thing that gets subtracted from v4 in observer_pass_time()
#  observed values:
#  0.13333334028720856
#  0.11666667461395264
#  0.10000000894069672
#  0.0833333358168602
#  0.06666667014360428
#  0.05000000447034836
#  0.03333333507180214    <-- idles here

# component_1 = read_u8(0x2E36BE)  # 1 unless time is stopped
# component_2 = read_float(0x2E3680)  # will always be between 0.0 and 1.0 while ingame

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


# TODO: need to list out some use cases here -- where would intentional collisions be useful, if we can just
#       search by parameters individually. My first thought was map variants (dammy vs. dammy pe)
def calculate_map_hash():
    """
    spawn locations and rotations
    item locations and rotations
    equipment locations and rotations
    portal locations and rotations
    map name
    map description
    some chosen tag data (e.g. spread values and other things that may have been changed in different versions)
    TODO: could also include a separate hash based only on locations as a way to suggest alternate map versions
            (also look at locality-sensitive hashing for this)
    TODO: define some kind of hash versioning (like borrowing the $1$deadbeef, $2$cafebabe format from pw hashes?)
    """

    pass


def calculate_match_hash():
    """
    map hash
    player hash
    game hash
    stored global random
    xbox names
    game start time? (see below)
    TODO: do we need two of these hashes? one with start time (xbox clock) and one without?
            The one without start time will be the same on each xbox, but will also be the same on map reruns
            The one with start time will be different on each xbox and different across map reruns
            Is there some additional match start time data that comes along with one of the map start packets from host?
    """

    pass


def calculate_game_hash():
    """
    game version strings
    overall xbe hash (or hash of some chosen regions of the xbe -- like map list?)
    """

    pass


def calculate_player_hash():
    """
    player names
    player sensitivities
    player control scheme
    player order (nonlocal ids)  <-- this should be excluded from individual hashes, and introduced in combined via the order of the individual hashes
    TODO: should this be individual player hashes or combined?
            probably individual hashes that get combined for the match hash
    """

    pass


def get_empty_player_meta():

    return dict(
            shots_by_weapon=defaultdict(int),
            damage_to_player=defaultdict(int),
            damage_from_player=defaultdict(int),
            kills_by_player=defaultdict(int),
            deaths_by_player=defaultdict(int),
            shots_by_tick=defaultdict(int),
            kills_by_tick=defaultdict(int),
            deaths_by_tick=defaultdict(int),
            assists_by_tick=defaultdict(int),
            damage_dealt_by_tick=defaultdict(int),
            damage_dealt=0,
            damage_received_by_tick=defaultdict(int),
            damage_received=0,
            camo_by_tick=defaultdict(int),
            camo_count=0,
            overshield_by_tick=defaultdict(int),
            overshield_count=0,
            active_projectiles=[]
        )


def initialize_meta_players(game_info):

    # TODO: time spent blocking ports, movement traveled, times ported

    # TODO: separate counters (value at current tick) and timelines (all historical values by tick)

    game_meta['players'] = {}

    for player in game_info['players']:
        game_meta['players'][player['player_index']] = get_empty_player_meta()

#
# def add_missing_meta_players(game_info):
#
#


def extract_events(old_game_info: dict, new_game_info: dict) -> list:

    # TODO: store full kill/death metadata (e.g. for each kill, store killer/killee locations, weapon, camo status, zoom, etc)

    # TODO: grenade trajectories

    # TODO: bullet impact spark locations

    # TODO: replace the current events list with a list of event objects containing event type and metadata

    # TODO: if last shot was current tick, check for either a hit or a new projectile owned by this player.
    # TODO: still need to handle the situation where two projectiles hit on the same tick (e.g. slow and fast projectiles from same player)

    # FIXME: loop through players once instead of for each type of check

    # TODO: button presses:
    #       - button hold durations (dead vs alive)
    #       - button press count (dead vs alive)
    #       - double melee timing (how many frames pass until grenade, then how many until second melee?)
    #       - pistol switch timing (how many frames after spawn until switch weapon is pressed?)
    #       - flashlight time

    events = []

    game_time = new_game_info['game_time_info']['game_time']

    if 'players' not in game_meta:
        initialize_meta_players(new_game_info)

    # new game
    if not old_game_info['game_engine_running'] and new_game_info['game_engine_running']:
        events.append(f'{game_time}: New game started on {new_game_info["multiplayer_map_name"]}')
        game_meta['start_time'] = new_game_info['current_time']
        initialize_meta_players(new_game_info)
        clear_caches()

    # projectiles
    # TODO: new projectiles this tick
    # TODO: deleted projectiles this tick
    # TODO: use this info to detect damage source
    # TODO: what happens when projectile is fired just after objects table is rearranged? (previous id may be some other projectile that was just deleted)
    #       -- need to make sure various attributes match as well, not just id
    new_projectile_ids_by_player = []
    deleted_projectile_ids_by_player = []
    if new_game_info['game_engine_can_score'] and new_game_info['game_engine_can_score'] and 'objects' in new_game_info:
        for i, object_id in enumerate(new_game_info['objects_meta']['object_ids_by_type']['projectile']):
            obj = new_game_info['objects'][new_game_info['objects_meta']['object_indexes_by_type']['projectile'][i]]
            # print(obj)
            if object_id not in old_game_info['objects_meta']['object_ids_by_type']['projectile']:
                # events.append(f'{game_time}: new projectile {object_id}')
                pass
        for i, object_id in enumerate(old_game_info['objects_meta']['object_ids_by_type']['projectile']):
            obj = old_game_info['objects'][old_game_info['objects_meta']['object_indexes_by_type']['projectile'][i]]
            if object_id not in new_game_info['objects_meta']['object_ids_by_type']['projectile']:
                # events.append(f'{game_time}: del projectile {object_id}')
                pass

    # shots fired, melees
    backslash = '\\'  # FIXME: workaround for backslashes in fstrings
    if new_game_info['game_engine_can_score'] and new_game_info['game_engine_can_score']:
        if len(old_game_info['players']) == len(new_game_info['players']):
            for old_player, new_player in zip(old_game_info['players'], new_game_info['players']):
                old_data = old_player['player_object_data']
                new_data = new_player['player_object_data']
                if old_data and new_data:

                    # weapons
                    for old_weapon, new_weapon in zip(old_data['weapons'], new_data['weapons']):
                        if old_weapon['object_id'] == new_weapon['object_id']:
                            if new_weapon['is_energy_weapon']:
                                old_ammo = old_weapon['charge_amount']
                                new_ammo = new_weapon['charge_amount']
                            else:
                                old_ammo = old_weapon['magazine_ammo_count']
                                new_ammo = new_weapon['magazine_ammo_count']
                            if old_ammo > new_ammo:
                                # events.append(f'{game_time}: {new_player["name"]} fired {new_weapon["tag_name"].split(backslash)[-1]} ({old_ammo} -> {new_ammo})')
                                game_meta['players'][new_player['player_index']]['shots_by_weapon'][new_weapon["tag_name"]] += 1 if new_weapon['is_energy_weapon'] else old_ammo - new_ammo
                                game_meta['players'][new_player['player_index']]['shots_by_tick'][game_time] += 1 if new_weapon['is_energy_weapon'] else old_ammo - new_ammo
                    if old_data['primary_nades'] > new_data['primary_nades']:
                        events.append(f'{game_time}: {new_player["name"]} threw frag grenade ({old_data["primary_nades"]} -> {new_data["primary_nades"]})')
                    if old_data['secondary_nades'] > new_data['secondary_nades']:
                        events.append(f'{game_time}: {new_player["name"]} threw plasma grenade ({old_data["secondary_nades"]} -> {new_data["secondary_nades"]})')

                    # melees
                    # FIXME: also need to handle melees after death by looking back at dead body's melee timer
                    # FIXME: melee damage happens on NEXT tick after melee
                    if not old_data['melee_impact_this_tick'] and new_data['melee_impact_this_tick']:
                        # events.append(f'{game_time}: {new_player["name"]} melee\'d')
                        pass

    # new damage
    # FIXME: two people can land the finishing shot on the same tick, and both get damage
    #         '66914: .minto damaged Hambone for 25.0',
    #         '66914: Donut damaged Hambone for 25.0',
    #         '66969: .minto damaged Hambone for 25.000003814697266',
    #         '66969: Donut damaged Hambone for 25.0',
    #         '66969: .minto got a kill (5)',
    #         '66969: Hambone died (3)',
    #         '66969: Donut got an assist (2)'
    #       if that happens, just give the damage to the one who got a kill?
    #       (but what if both get a kill on diff players, e.g. splash damage?)
    # FIXME: sometimes final damage isn't counted as an event (maybe skipped or overlapping ticks?)
    # TODO: handle case where player had a charging OS and died from a backwhack (if we exclude OS-invuln-damage)
    # NOTE: damage reported in assist damagers table is PRE-multiplier
    #           (e.g. PR does 2x damage to shields (26) but shows as 13 in damagers table)
    #       bleedthrough is also weird here -- damager table gets full shield damage (into negative shield) + bleedthrough health damage
    #           (e.g. with 5 health, and AR bullet does 10 damage (5 shield + 5 health), but gets recorded in table as 15 damage (10 shield + 5 health))
    # TODO: get multipliers from tags and do those calculations on the fly, see above
    # TODO: find if there's an indicator of melee damage attempts (e.g. 0 changes to 1 on frame where melee hits)
    # TODO: check if damaging player has an active melee/nade/projectile, or has just fired this tick
    #       -- if so, try to guess damage type/weapon
    #       -- if multiple options, show options in log
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
                            # FIXME: clean this up, lots of duplication here
                            game_meta['players'][damage_dealer]['damage_dealt_by_tick'][game_time] += new_amount - old_amount
                            game_meta['players'][damage_receiver]['damage_received_by_tick'][game_time] += new_amount - old_amount
                            game_meta['players'][damage_dealer]['damage_to_player'][damage_receiver] += new_amount - old_amount
                            game_meta['players'][damage_receiver]['damage_from_player'][damage_dealer] += new_amount - old_amount
                            game_meta['players'][damage_dealer]['damage_dealt'] += new_amount - old_amount
                            game_meta['players'][damage_receiver]['damage_received'] += new_amount - old_amount
                    else:
                        events.append(f'{game_time}: {damage_dealer_name} damaged {damage_receiver_name} for {new_amount}')
                        game_meta['players'][damage_dealer]['damage_dealt_by_tick'][game_time] += new_amount
                        game_meta['players'][damage_receiver]['damage_received_by_tick'][game_time] += new_amount
                        game_meta['players'][damage_dealer]['damage_to_player'][damage_receiver] += new_amount
                        game_meta['players'][damage_receiver]['damage_from_player'][damage_dealer] += new_amount
                        game_meta['players'][damage_dealer]['damage_dealt'] += new_amount
                        game_meta['players'][damage_receiver]['damage_received'] += new_amount
            else:
                for damage_receiver, new_amount in damage_receivers.items():
                    damage_receiver_name = new_game_info['players'][damage_receiver]['name']
                    events.append(f'{game_time}: {damage_dealer_name} damaged {damage_receiver_name} for {new_amount}')
                    game_meta['players'][damage_dealer]['damage_dealt_by_tick'][game_time] += new_amount
                    game_meta['players'][damage_receiver]['damage_received_by_tick'][game_time] += new_amount
                    game_meta['players'][damage_dealer]['damage_to_player'][damage_receiver] += new_amount
                    game_meta['players'][damage_receiver]['damage_from_player'][damage_dealer] += new_amount
                    game_meta['players'][damage_dealer]['damage_dealt'] += new_amount
                    game_meta['players'][damage_receiver]['damage_received'] += new_amount

    # kills, deaths, assists, powerups
    if old_game_info['game_engine_running'] and new_game_info['game_engine_running']:
        if len(old_game_info['players']) == len(new_game_info['players']):
            for old_player, new_player in zip(old_game_info['players'], new_game_info['players']):
                if (kills := new_player['kills']) > old_player['kills']:
                    events.append(f'{game_time}: {new_player["name"]} got a kill ({kills})')
                    game_meta['players'][new_player['player_index']]['kills_by_tick'][game_time] += kills - old_player['kills']
                if (deaths := new_player['deaths']) > old_player['deaths']:
                    events.append(f'{game_time}: {new_player["name"]} died ({deaths})')
                    game_meta['players'][new_player['player_index']]['deaths_by_tick'][game_time] += deaths - old_player['deaths']

                    # predict assist
                    # FIXME: not accurate, assists are based on damage table before killing player's damage is applied
                    # max_recent_damage = 0
                    # recent_damages = []
                    # for damage_table_entry in new_player['damage_table']:
                    #     if damage_table_entry['damage_time'] > game_time - 180:
                    #         recent_damages.append(damage_table_entry['damage_amount'])
                    #         if damage_table_entry['damage_amount'] > max_recent_damage:
                    #             max_recent_damage = damage_table_entry['damage_amount']
                    # damage_threshold = max_recent_damage * 0.4
                    # # for damage_table_entry in new_player['damage_table']:
                    # #     if damage_table_entry['damage_amount'] > damage_threshold:
                    # events.append(f'{game_time}: damage threshold was {damage_threshold} and recent damages were {recent_damages}')

                if (assists := new_player['assists']) > old_player['assists']:
                    events.append(f'{game_time}: {new_player["name"]} got an assist ({assists})')
                    game_meta['players'][new_player['player_index']]['assists_by_tick'][game_time] += assists - old_player['assists']

                # camo
                if new_player['derived_stats']['has_camo'] and not old_player['derived_stats']['has_camo']:
                    events.append(f'{game_time}: {new_player["name"]} picked up camo')
                    game_meta['players'][new_player['player_index']]['camo_by_tick'][game_time] += 1
                    game_meta['players'][new_player['player_index']]['camo_count'] += 1
                if not new_player['derived_stats']['has_camo'] and old_player['derived_stats']['has_camo']:
                    events.append(f'{game_time}: {new_player["name"]} lost camo')

                # overshield
                # FIXME: what if a player picks up an overshield and gets backwhacked that same tick?
                #        seems like kill happens before powerup pickup -- player will die on the same frame as overshield spawns if standing on it
                if new_player['derived_stats']['has_overshield'] and not old_player['derived_stats']['has_overshield']:
                    events.append(f'{game_time}: {new_player["name"]} picked up overshield')
                    game_meta['players'][new_player['player_index']]['overshield_by_tick'][game_time] += 1
                    game_meta['players'][new_player['player_index']]['overshield_count'] += 1
                if not new_player['derived_stats']['has_overshield'] and old_player['derived_stats']['has_overshield']:
                    events.append(f'{game_time}: {new_player["name"]} lost overshield')

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

    # re-inject aggregates into derived stats for all players
    # FIXME: this doesn't capture first and last tick -- do this in main loop after extract_events instead?
    # if new_game_info['game_engine_can_score']:
    #     for player in new_game_info['players']:
    #         meta = game_meta['players'][player['player_index']]
    #         attributes_to_copy = [
    #             'shots_by_weapon',
    #             'damage_to_player',
    #             'damage_from_player',
    #             'kills_by_player',
    #             'deaths_by_player',
    #             'damage_dealt',
    #             'damage_received',
    #             'camo_count',
    #             'overshield_count',
    #         ]
    #         for attribute in attributes_to_copy:
    #             player['derived_stats'][attribute] = meta[attribute]

    # game over
    if old_game_info['game_engine_can_score'] and not new_game_info['game_engine_can_score']:
        events.append(f'{game_time}: Game ended on {new_game_info["multiplayer_map_name"]}')
        game_meta['start_time'] = None
        new_game_info['game_ended_this_tick'] = True

        # we're only generating game ids while the game is active, so make sure the last tick also has an id
        # FIXME: does this mean damage on the game over tick will count? (in cases where last kill happens on different tick than game over)
        new_game_info['game_id'] = old_game_info['game_id']

    new_game_info['game_meta'] = game_meta

    # postgame
    # FIXME: need to tie this to something other than game_engine_running
    # if old_game_info['game_engine_running'] and not new_game_info['game_engine_running']:
    #     events.append(f'{game_time}: Game engine stopped')

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


def process_write_queue():

    # TODO: keep a log of all modifications this session?
    while write_queue_from_ui.qsize() > 0:

        write_data = write_queue_from_ui.get(block=False)
        print(f'about to write: {write_data}')
        address = int(write_data['address'], 0)
        length = int(write_data['length'], 0)
        value = int(write_data['value'], 0).to_bytes(byteorder='little', length=length)

        print('before:', get_formatted_bytes(0x9c514, 2))
        write_bytes(address, value, length)
        print('after: ', get_formatted_bytes(0x9c514, 2))

# from pympler import tracker
# tr = tracker.SummaryTracker()


def get_summary(game_info):
    """
    This summary gets sent periodically to the halospawns api for the "games in progress" page.
    """

    return dict(
        game_id=game_info['game_id'],
        status=dict(
            game_time=game_info['game_time_info']['game_time'],
            map_name=game_info['multiplayer_map_name'],
            players=[dict(
                name=player['name'],
                team=player['team'],
                score=player['score'],
                kills=player['kills'],
                deaths=player['deaths'],
                assists=player['assists'],
                damage_dealt=game_info['game_meta']['players'][player['player_index']]['damage_dealt'] if 'game_meta' in game_info else 0,
                damage_received=game_info['game_meta']['players'][player['player_index']]['damage_received'] if 'game_meta' in game_info else 0,
            ) for player in game_info['players']],
        )
    )


def main_loop():
    """
    Basic flow is:
    - read game_time as quickly as we can, looking for a change
    - if game_time has changed:
        - read game info from memory
        - offload game info to background handler threads (websockets, database, local file, etc)
    """

    # memory_benchmark()

    counter = 0
    global pymem_counter
    # pymem_counter = 0
    last = 0
    last_game_time = 0
    last_real_time = datetime.datetime.now()
    last_post_steps = 0
    benchmark_tick_count = 0
    benchmark_loop_count = 0
    last_game_info = {}
    events = []
    duration_total = 0
    last_status_sent = 0

    print(f'game_time host address: {hex(get_host_address(game_time_address))}')

    while True:

        try:

            game_time = read_u32(game_time_address) - 1  # game_time is incremented after the tick, so we want time-1

            benchmark_loop_count += 1

            counter += 1
            if game_time != last_game_time:

                benchmark_tick_count += 1
                real_time = datetime.datetime.now()
                # print(f'{real_time} new tick: {game_time} with {counter} measurements and {pymem_counter} pymem reads last tick (avg {benchmark_loop_count/benchmark_tick_count:0.2f} loops/tick)')
                counter = 0
                pymem_counter = 0

                # TODO: keep a queue of memory snapshots / game_info operations so we don't miss ticks
                populate_memory_cache()

                # TODO: decide where in the process this should live...
                #       maybe after get_game_info? since our writes will really be affecting the next tick,
                #       not the one we're reading in get_game_info
                process_write_queue()

                game_info = get_game_info()

                invalidate_memory_cache()

                if game_info['game_time_info']['game_time'] != game_time:
                    print(f"  WARNING: mismatched game time (expected {game_time}, got {game_info['game_time_info']['game_time']})")

                # Python 3.9: ~11.54ms | Python 3.11: ~10.02ms
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

                game_info['performance'] = dict(
                    game_info_time=duration/1000,
                    loop_time=(real_time - last_real_time).microseconds/1000,
                    post_steps_ms=last_post_steps,
                    # known_addresses=len(known_addresses),
                    memory_mbytes=psutil.Process(os.getpid()).memory_info().vms / 1024 ** 2,
                )

                last_real_time = real_time
                post_steps_start = datetime.datetime.now()

                # if benchmark_tick_count == 30*300:
                #     analyze_offset_map()
                #     break

                # print full game info for pregame and first tick
                # if game_time < 1:
                #     pprint(game_info)

                # TODO: PERF: check performance impact of deepcopying this every time
                # deep copy is needed since we're modifying game_info on the other side of the queue
                game_info_queue.put(copy.deepcopy(game_info))
                game_info_queue_for_ui.put(game_info)

                if WS_RELAY_ENABLED:
                    ws_client_queue.put(game_info)

                if (game_time > last_status_sent + API_UPDATE_INTERVAL_SECONDS and game_info['game_engine_can_score']) or game_info['game_ended_this_tick']:
                    api_client_queue.put(get_summary(game_info))
                    last_status_sent = game_time
                elif game_time < last_status_sent:
                    last_status_sent = 0

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

                    # websockets optimization:
                    #   http://buildnewgames.com/optimizing-websockets-bandwidth/
                    #   http://buildnewgames.com/real-time-multiplayer/

                    # webtransport:
                    #    https://github.com/aiortc/aioquic
                    #    https://github.com/django/asgiref/issues/280
                    #    https://github.com/aiortc/aioquic/issues/163
                    #    https://github.com/Sh3B0/realtime-web

                    # TODO: this should also be in a background thread
                    for client in clients:
                        client.sendMessage(data)

                # if last_post_steps == 0:
                #     analyze_offset_map()

                last_post_steps = (datetime.datetime.now() - post_steps_start).microseconds/1000

                # if benchmark_tick_count > 600:
                #     tr.print_diff()

            last_game_time = game_time

        except ValueError as e:

            # this typically happens when trying to read unmapped memory, e.g. read(0x0)
            pprint(e)
            clear_caches()
            wait_for_xemu()
            # raise

        except MemoryReadError as e:

            pprint(e)
            clear_caches()
            # FIXME: clearing caches is not enough -- likely also need to reset all the global initial reads
            # FIXME: need to add a check to make sure this is really due xemu being closed, and not a real read error
            #        maybe check pymem.exception.WinAPIError: Windows api error, error_code: 299
            wait_for_xemu()

        except KeyError as e:

            pprint(e)
            raise

        except socket.timeout as e:

            print('DROPPED FRAME DUE TO SOCKET TIMEOUT')
            t._qmp.close()
            t.connect()
            # t._qmp.connect()

        else:

            pass


if __name__ == '__main__':

    # disable garbage collection to avoid periodic freezes during gameplay
    # garbage collector is manually run at the end of each game
    # gc.set_debug(gc.DEBUG_STATS)
    gc.disable()

    # main_thread = threading.Thread(target=main_loop, daemon=True, name='main_loop_thread')
    # main_thread.start()

    ui_thread = threading.Thread(target=ui.start_ui, args=(game_info_queue_for_ui,write_queue_from_ui,), daemon=True, name='ui_thread')
    ui_thread.start()

    api_client_thread = threading.Thread(target=api_client.start_client, args=(api_client_queue,), daemon=True, name='api_client_thread')
    api_client_thread.start()

    ws_client_thread = threading.Thread(
        target=ws_client.start_client,
        args=(ws_client_queue,),
        kwargs={
            'host': WS_RELAY_BASE_URL,
            'room': 'test-room',
        },
        daemon=True,
        name='ws_client_thread'
    )
    ws_client_thread.start()

    # asyncio.run(main_loop())
    main_loop()

    # ui.start_ui(game_info_queue_for_ui)
