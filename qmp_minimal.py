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
