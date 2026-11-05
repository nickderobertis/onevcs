#!/usr/bin/env python3
"""External GitHub HTTP boundary plus the gh executable's HTTP forwarding seam."""
import http.server
import io
import json
import os
from pathlib import Path
import sys
import urllib.error
import urllib.request
import zipfile

if len(sys.argv) > 1 and sys.argv[1] == 'serve':
    root = Path(sys.argv[2])

    class Host(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            self.answer()

        def do_POST(self):
            self.answer()

        def do_PATCH(self):
            self.answer()

        def answer(self):
            body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
            payload = json.loads(body) if body else {}
            state = json.loads((root / 'state.json').read_text())
            with (root / 'requests.jsonl').open('a') as log:
                log.write(json.dumps({'method': self.command, 'path': self.path, 'payload': payload,
                                      'token': self.headers.get('Authorization')}) + '\n')
            page = int(self.path.split('page=')[-1]) if '&page=' in self.path else 1
            status = state.get('status', 200)
            if '/issues/' in self.path and self.command in ('POST', 'PATCH'):
                status = state.get('write_status', status)
            result = {'message': state.get('message', 'Bad credentials')}
            binary = None
            if status == 200:
                if self.path == '/graphql':
                    variables = payload['variables']
                    assert variables['owner'] == 'owner' and variables['name'] == 'repo'
                    assert variables['number'] == 1
                    for field in ['viewer', 'login', 'headRefOid', 'comments', 'pageInfo', 'nodes', 'databaseId', 'author']:
                        assert field in payload['query'], field
                    comments = [dict(id=c['node_id'], databaseId=c['id'], url=c['html_url'],
                                     body=c['body'], author={'login': c['author']}) for c in state['comments']]
                    offset = 0 if state.get('repeat_cursor') else int(variables.get('after', 'cursor:0').split(':')[1])
                    size = state.get('comment_page_size', 100)
                    more = offset + size < len(comments)
                    comments = comments[offset:offset + size]
                    result = {'data': {'viewer': {'login': 'viewer'}, 'repository': {'pullRequest': {
                        'headRefOid': state['head'], 'comments': {'nodes': comments,
                            'pageInfo': {'hasNextPage': more, 'endCursor': 'cursor:1' if state.get('repeat_cursor') else ('cursor:' + str(offset + size) if more else None)}}}}}}
                    result = state.get('graphql_override', result)
                    if 'graphql_raw' in state:
                        binary = state['graphql_raw'].encode()
                elif '/issues/' in self.path:
                    if self.command == 'POST':
                        comment = dict(id=100 + len(state['comments']), node_id='owned-new',
                                       html_url='https://github.com/owner/repo/pull/1#issuecomment-' + str(100 + len(state['comments'])), author='viewer')
                        state['comments'].append(comment)
                    else:
                        comment = next(c for c in state['comments'] if str(c['id']) == self.path.split('/')[-1])
                    comment['body'] = payload['body']
                    result = comment
                    (root / 'state.json').write_text(json.dumps(state))
                    result = state.get('write_override', result)
                    if 'write_raw' in state:
                        binary = state['write_raw'].encode()
                elif '/workflows/' in self.path:
                    result = {'workflow_runs': state['runs']}
                elif '/runs/' in self.path:
                    result = {'artifacts': state.get('artifact_pages', [state['artifacts']])[page - 1]}
                elif self.path.endswith('/zip'):
                    self.send_response(302)
                    self.send_header('Location', '/objects/archive')
                    self.send_header('Content-Length', '0')
                    self.end_headers()
                    return
                elif self.path == '/objects/archive':
                    archive = io.BytesIO()
                    with zipfile.ZipFile(archive, 'w') as z:
                        z.writestr('history.json', b'{"saved":true}\n\x00\xff')
                    binary = archive.getvalue()
                else:
                    status, result = 404, {'message': 'Not Found'}
            data = binary if binary is not None else json.dumps(result).encode()
            self.send_response(status)
            if '/runs/' in self.path and state.get('artifact_pages') and page < len(state['artifact_pages']):
                self.send_header('Link', '<https://api.github.com' + self.path.split('&page=')[0] + '&page=' + str(page + 1) + '>; rel="next"')
            if '/workflows/' in self.path:
                self.send_header('Link', '<https://api.github.com/forbidden-next-page>; rel="next"')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = http.server.HTTPServer(('127.0.0.1', 0), Host)
    (root / 'port.ready').write_text(str(server.server_port))
    (root / 'port.ready').replace(root / 'port')
    server.serve_forever()
else:
    args = sys.argv[1:]
    assert args.pop(0) == 'api', args
    include = '--include' in args
    method = 'GET'
    fields = {}
    path = None
    while args:
        arg = args.pop(0)
        if arg in ('--method', '-X'):
            method = args.pop(0)
        elif arg in ('-f', '-F'):
            key, value = args.pop(0).split('=', 1)
            fields[key] = int(value) if arg == '-F' and value.isdigit() else value
        elif arg != '--include':
            path = arg
    if path.startswith('https://api.github.com/'):
        path = path.removeprefix('https://api.github.com/')
    if path == 'graphql':
        method = 'POST'
        query = fields.pop('query')
        fields = {'query': query, 'variables': fields}
    request = urllib.request.Request(os.environ['SESSIONLESS_HOST'] + '/' + path,
        data=json.dumps(fields).encode() if fields else None,
        headers={'Authorization': 'Bearer ' + os.environ.get('GH_TOKEN', '')}, method=method)
    try:
        response = urllib.request.urlopen(request)
    except urllib.error.HTTPError as error:
        response = error
    if include:
        sys.stdout.buffer.write(('HTTP/1.1 ' + str(response.status) + ' Host\r\n').encode())
        for key, value in response.headers.items():
            sys.stdout.buffer.write((key + ': ' + value + '\r\n').encode())
        sys.stdout.buffer.write(b'\r\n')
    data = response.read()
    sys.stdout.buffer.write(data)
    if response.status >= 400:
        sys.stderr.write('HTTP ' + str(response.status) + ': ' + data.decode())
        sys.exit(1)
