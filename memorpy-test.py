from memorpy import MemWorker

import psutil


def get_pid(pname):
    for proc in psutil.process_iter():
        if proc.name() == pname:
            return proc.pid
    return None


PROCNAME = "xqemu.exe"
pid = get_pid(PROCNAME)
mw = MemWorker(pid=pid)

addresses = {
    0x11000:  ('51 A1 48 36 2E 00 69 C0 0D 66 19 00 05 5F F3 6E 3C 8B C8 C1 E9 10 89 4C 24 00 A3 48 36 2E 00 DB', 0x132d1000),
    0x109888: ('39 1D 3C AB 2F 00 74 06 89 1D 3C AB 2F 00 39 1D 38 AB 2F 00 74 06 89 1D 38 AB 2F 00 39 1D 48 AB', 0x133c9888),
    0x155869: ('33 C9 66 8B 4D 38 25 FF FF 00 00 8D 14 40 A1 AC C6 2F 00 8D B5 BC 00 00 00 51 8B 48 34 8B 54 91', 0x13415869),
    0x1D1D93: ('A3 C0 27 26 00 8B 48 10 89 0D BC 27 26 00 8A 48 05 88 0D 42 27 26 00 C6 05 43 27 26 00 80 C6 00', 0x13491d93),
    0x1FFE08: ('6D 61 63 68 69 6E 65 20 23 25 64 20 68 61 73 20 73 75 63 63 65 73 73 66 75 6C 6C 79 20 73 77 69', 0x134bfe08),
    0x2338CC: ('78 62 6F 78 20 65 74 68 65 72 6E 65 74 20 6C 69 6E 6B 20 69 73 20 25 73 25 73 25 73 25 73 25 73', 0x134f38cc),
    0x25F5B4: ('77 72 6F 6E 67 20 73 74 72 75 63 74 75 72 65 20 62 73 70 2C 20 63 61 6E 6E 6F 74 20 65 78 65 63', 0x1351f5b4)
}

# def read_u32(address)


def byte_seq_search(byte_seq):

    return list(mw.mem_search(bytearray.fromhex(byte_seq)))


def get_base_addr(mw):

    identifier = bytearray.fromhex('51 A1 48 36 2E 00 69 C0 0D 66 19 00')  # 0x11000 and 0x11200
    matches = list(mw.mem_search(identifier))
    if matches:
        return matches[0] - 0x11000


if __name__ == '__main__':

    base_addr = get_base_addr(mw)
    print('Base address is {}'.format(base_addr))
    # message = base_addr + 0x2338BC
    # message.dump()
    # print(base_addr)

    last_addr = 0
    last_match = base_addr.value
    last_hva = base_addr.value

    for expected_address, (expected_bytes, hva) in addresses.items():

        matches = byte_seq_search(expected_bytes)
        # print(hex(expected_address), matches, hex(matches[-1].value))
        for match in matches:
            print('{}|{}|{}:   last diff xbe: {}   last diff xqemu: {}   diff of diffs: {}   hva2xqemu: {}   diff of hva: {}'.format(
                hex(expected_address),
                hex(match.value),
                hex(hva),
                hex(expected_address - last_addr),
                hex(match.value - last_match),
                hex((match.value - last_match) - (expected_address - last_addr)),
                hex(match.value - hva),
                hex(hva - last_hva)))
        print()
        last_addr = expected_address
        last_match = matches[-1].value
        last_hva = hva

