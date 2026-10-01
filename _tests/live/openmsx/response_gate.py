"""Test-only TCP relay that withholds one real producer reply at a tap boundary."""
import json
import selectors
import socket
import threading
import time


class ResponseGate:
    def __init__(self, broker_port):
        self.listener = socket.socket()
        self.listener.bind(('127.0.0.1', 0))
        self.listener.listen(1)
        self.listener.settimeout(.1)
        self.port = self.listener.getsockname()[1]
        self.broker_port = broker_port
        self.boundary = None
        self.held = None
        self.rows = []
        self.error = None
        self.reached = threading.Event()
        self.stopped = threading.Event()
        self.closing = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def matches(self, request, response):
        if not self.boundary or not response.get('ok'):
            return False
        params = request.get('params', {})
        if not params.get('_temporal_owner'):
            return False
        method = request.get('method')
        if self.boundary == 'press_completed':
            return method == 'step' and response.get('result', {}).get('status') == 'completed'
        return method == 'set_input' and bool(params.get('buttons')) == (self.boundary == 'input_pressed')

    def run(self):
        try:
            # MCP probes endpoint availability before creating its attached link.
            while not self.closing.is_set():
                try:
                    client, _ = self.listener.accept()
                except socket.timeout:
                    continue
                self.serve(client)
                if self.held is not None:
                    return
        except BaseException as error:
            if not self.closing.is_set():
                self.error = repr(error)
        finally:
            self.listener.close()
            self.stopped.set()

    def serve(self, client):
        peers = []
        try:
            peers.append(client)
            broker = socket.create_connection(('127.0.0.1', self.broker_port), timeout=1)
            peers.append(broker)
            requests = {}
            buffers = {client: b'', broker: b''}
            with selectors.DefaultSelector() as selector:
                for peer in peers:
                    peer.settimeout(1)
                    selector.register(peer, selectors.EVENT_READ)
                while not self.closing.is_set():
                    for key, _ in selector.select(.01):
                        peer = key.fileobj
                        data = peer.recv(65536)
                        if not data:
                            self.rows.append({'event':'eof', 'side':'mcp' if peer is client else 'broker',
                                              'time':time.monotonic()})
                            return
                        buffers[peer] += data
                        assert len(buffers[peer]) <= 1048576, 'relay frame budget'
                        while b'\n' in buffers[peer]:
                            line, buffers[peer] = buffers[peer].split(b'\n', 1)
                            value = json.loads(line)
                            direction = 'request' if peer is client else 'response'
                            self.rows.append({'event':direction, 'value':value, 'time':time.monotonic()})
                            if peer is client:
                                requests[value['id']] = value
                                broker.sendall(line + b'\n')
                            else:
                                request = requests.get(value.get('id'), {})
                                if self.held is None and self.matches(request, value):
                                    self.held = {'request':request,'response':value,'time':time.monotonic()}
                                    self.rows.append({'event':'withheld', **self.held})
                                    self.reached.set()
                                else:
                                    client.sendall(line + b'\n')
        finally:
            for peer in peers:
                peer.close()

    def close(self):
        self.closing.set()
        self.thread.join(timeout=2)
        assert not self.thread.is_alive(), 'relay did not stop'
