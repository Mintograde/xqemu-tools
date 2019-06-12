import ctypes as c
from ctypes import wintypes as w
from struct import *
from time import *
import datetime
import sys

pid = 18572

k32 = c.windll.kernel32

OpenProcess = k32.OpenProcess
OpenProcess.argtypes = [w.DWORD,w.BOOL,w.DWORD]
OpenProcess.restype = w.HANDLE

ReadProcessMemory = k32.ReadProcessMemory
ReadProcessMemory.argtypes = [w.HANDLE,w.LPCVOID,w.LPVOID,c.c_size_t,c.POINTER(c.c_size_t)]
ReadProcessMemory.restype = w.BOOL
PAA = 0x1F0FFF
address = 0x4000000
ph = OpenProcess(PAA,False,int(pid)) #program handle

buff = c.create_string_buffer(4)
bufferSize = (c.sizeof(buff))
bytesRead = c.c_ulonglong(0)

addresses_list = xrange(address,0x9000000,0x4)
log=open(r'out.txt.','wb',0)
for i in addresses_list:
    ReadProcessMemory(ph, c.c_void_p(i), buff, bufferSize, c.byref(bytesRead))
    value = unpack('I',buff)[0]
    if value == int(sys.argv[1]):
        log.write('%x\r\n' % (i, ))






# datum array header fields
datum_array_name_offset = 0x0    # 32 bytes of null-terminated ASCII
datum_array_first_element_pointer_offset = 0x34    # u32
datum_array_element_max_count_offset = 0x20
datum_array_element_size_offset = 0x22    # u16
# todo: active element count, total array size, etc.

# get player datum array info
player_datum_array = read_u32(0x2FAD28)
player_datum_array_max_count = read_u16(player_datum_array + datum_array_element_max_count_offset)
player_datum_array_element_size = read_u16(player_datum_array + datum_array_element_size_offset)
player_datum_array_first_element_address = read_u32(player_datum_array + datum_array_first_element_pointer_offset)

# get first player data
player_index = 0
player_datum = read(player_datum_array_first_element_address + player_index * player_datum_array_element_size, player_datum_array_element_size)




# player object
player_object_handle = read_u32(player_datum_array_first_element_address + 0x34)
object_header_datum_array = read_u32(0x2FC6AC)
object_Header_datum_array_first_element_address = read_u32(object_header_datum_array + 0x34)
object_data_address = read_u32(object_Header_datum_array_first_element_address + (player_object_handle & 0xFFFF) * 16 + 8)