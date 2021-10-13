"""
See https://discord.com/channels/680221390359887933/722155756622708744/817878951530594354
    https://discord.com/channels/680221390359887933/722155756622708744/817896747951063062

         Mintograde: Silly question about qmp from python:

            Has anyone else run into cases where xemu (presumably) sends a qmp response before qmp.py can begin listening for the response? I assume this is an indirect result of the recent performance increases (or some upstream qemu improvements), as I never had any issues back when the framerates were low -- good problem to have I suppose.

            These are the relevant sections of qmp.py:
            - https://github.com/mborgerson/xemu/blob/master/python/qemu/qmp.py#L246-L247
            - https://github.com/mborgerson/xemu/blob/master/python/qemu/qmp.py#L137

            Eventually, after enough sent commands, I'll hit one that just hangs on qmp.py's __sockfile.readline() line. I can tell it's still waiting, because I can pause execution in xemu and see the STOP event come in -- so my assumption is that I've just missed the previous response.

            I'm wondering what can be done to avoid that. I'm sending memory read commands, so I'd like to avoid ignoring those responses if possible. (I'm currently working around it by setting a timeout, but it's happening so frequently (every few hundred commands) that I'm hoping there's a more robust solution than dropping data).

         Mintograde: I'll usually get through a couple hundred commands before I hit a timeout
"""

import socket

from qmp import QEMUMonitorProtocol

q = QEMUMonitorProtocol(('localhost', 4444))
q.connect()
q.settimeout(0.5)

command = {'execute': 'human-monitor-command', 'arguments': {'command-line': 'x /4xb 3124520'}}
count = 0

while True:
    try:
        count += 1
        response = q.cmd_obj(command)
        # print(f'{response} {count}')
    except socket.timeout:
        print(f'Timed out on command {count}')
        break

    import time
    # if count % 2 == 0:
    time.sleep(0.002)
