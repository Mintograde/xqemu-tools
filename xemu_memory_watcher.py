import re

import psutil

from qmp import QEMUMonitorProtocol


class QMPMessenger:

    def __init__(self, address, port):

        self.instance = None
        self.address = address
        self.port = port
        self.connect()

    def connect(self):

        self.instance = QEMUMonitorProtocol((self.address, self.port))
        self.instance.connect()
        self.instance.settimeout(0.5)

    def reconnect(self):

        self.instance.close()
        self.connect()


class XemuInstance:
    """

    """

    def __init__(self, pid=None, qmp_address=None, qmp_port=None):

        self.pid = pid

        if qmp_address and qmp_port:
            self.qmp_instance = QMPMessenger(qmp_address, qmp_port)
        else:
            self.qmp_instance = None

        self.watched_addresses = {}


if __name__ == '__main__':

    instances = []
    for proc in psutil.process_iter():
        if proc.name() == 'xemuw.exe':
            info = proc.as_dict()
            cmdline = ' '.join(info['cmdline'])
            match = re.search(r'-qmp tcp:(?P<address>.+):(?P<port>\d+),', cmdline)
            if match:
                instances.append(XemuInstance(pid=proc.pid,
                                              qmp_address=match.group('address'),
                                              qmp_port=match.group('port')))
            else:
                continue
