#!/usr/bin/env python3
"""Steam QR login for the pack importer. No password, no TTY.

  python3 tools/steam_qr.py login   # pops a QR png; scan with the Steam mobile app
  python3 tools/steam_qr.py test    # CM-login with the saved token, fetch a test manifest

`login` runs BeginAuthSessionViaQR against the public WebAPI, renders the
challenge URL as a QR image (opened in Preview), and polls until the phone
approves. The refresh token lands in ~/.config/smash/steam_token.json (0600)
and is what roa_get.py uses to download workshop content headlessly.

Deps: steam, qrcode (both in tools/.venv via fetch_packs bootstrap, or any venv).
"""
import base64
import json
import pathlib
import subprocess
import sys
import time
import urllib.parse
import urllib.request

from steam.protobufs.steammessages_auth_pb2 import (
    CAuthentication_BeginAuthSessionViaQR_Request,
    CAuthentication_BeginAuthSessionViaQR_Response,
    CAuthentication_PollAuthSessionStatus_Request,
    CAuthentication_PollAuthSessionStatus_Response,
)

TOKEN_FILE = pathlib.Path.home() / ".config" / "smash" / "steam_token.json"
API = "https://api.steampowered.com/IAuthenticationService/{}/v1/"


def service_call(method, req, resp_cls):
    data = urllib.parse.urlencode(
        {"input_protobuf_encoded": base64.b64encode(req.SerializeToString()).decode()}
    ).encode()
    r = urllib.request.urlopen(urllib.request.Request(API.format(method), data=data), timeout=15)
    resp = resp_cls()
    resp.ParseFromString(r.read())
    return resp


def jwt_payload(tok):
    pad = lambda s: s + "=" * (-len(s) % 4)
    return json.loads(base64.urlsafe_b64decode(pad(tok.split(".")[1])))


def cmd_login():
    import qrcode
    begin = CAuthentication_BeginAuthSessionViaQR_Request()
    begin.device_friendly_name = "smash-pack-importer"
    begin.platform_type = 1  # k_EAuthTokenPlatformType_SteamClient -> client-audience token
    sess = service_call("BeginAuthSessionViaQR", begin, CAuthentication_BeginAuthSessionViaQR_Response)
    print(f"challenge: {sess.challenge_url}")

    png = pathlib.Path("/tmp/steam_qr.png")
    qrcode.make(sess.challenge_url).save(png)
    subprocess.run(["open", str(png)], check=False)
    print(f"QR opened ({png}). Scan with the Steam mobile app and approve.")

    interval = sess.interval or 5.0
    deadline = time.time() + 180
    while time.time() < deadline:
        time.sleep(interval)
        poll = CAuthentication_PollAuthSessionStatus_Request()
        poll.client_id = sess.client_id
        poll.request_id = sess.request_id
        st = service_call("PollAuthSessionStatus", poll, CAuthentication_PollAuthSessionStatus_Response)
        if st.refresh_token:
            steamid = jwt_payload(st.refresh_token)["sub"]
            TOKEN_FILE.parent.mkdir(parents=True, exist_ok=True)
            TOKEN_FILE.write_text(json.dumps(
                {"account_name": st.account_name, "refresh_token": st.refresh_token,
                 "steamid": steamid}, indent=2) + "\n")
            TOKEN_FILE.chmod(0o600)
            print(f"logged in as {st.account_name} ({steamid}); token -> {TOKEN_FILE}")
            return 0
        if st.new_client_id:
            sess.client_id = st.new_client_id
    print("timed out waiting for approval (180s); rerun login")
    return 1


def patch_zstd_chunks():
    """steam 1.4.4 predates Valve's zstd depot chunks; newer workshop uploads use them and the
    stock get_chunk falls through to ZipFile -> BadZipFile. Layout per SteamKit VZstdUtil.cs:
    b'VSZa' + crc32(4) + zstd frame + 15B footer [crc32(4) size(4) ?(4) b'zsv']."""
    import struct
    import zlib
    from io import BytesIO
    from zipfile import ZipFile

    import zstandard
    from steam.client.cdn import CDNClient
    from steam.core.crypto import symmetric_decrypt
    from steam.exceptions import SteamError

    if getattr(CDNClient.get_chunk, "_vsz_patched", False):
        return
    orig = CDNClient.get_chunk

    def get_chunk(self, app_id, depot_id, chunk_id):
        if (depot_id, chunk_id) not in self._chunk_cache:
            resp = self.cdn_cmd('depot', '%s/chunk/%s' % (depot_id, chunk_id))
            data = symmetric_decrypt(resp.content, self.get_depot_key(app_id, depot_id))
            if data[:4] == b'VSZa':
                if data[-3:] != b'zsv':
                    raise SteamError("VSZ: invalid footer: %r" % data[-3:])
                crc, size = struct.unpack('<II', data[-15:-7])
                data = zstandard.ZstdDecompressor().decompress(data[8:-15], max_output_size=size)
                if len(data) != size or zlib.crc32(data) != crc:
                    raise SteamError("VSZ: size/CRC32 mismatch for decompressed data")
                self._chunk_cache[(depot_id, chunk_id)] = data
            elif data[:2] == b'VZ':
                # decrypting twice would be nice to avoid, but orig re-fetches via its own
                # cdn_cmd anyway and the LZMA path is the rare legacy case now
                return orig(self, app_id, depot_id, chunk_id)
            else:
                with ZipFile(BytesIO(data)) as zf:
                    self._chunk_cache[(depot_id, chunk_id)] = zf.read(zf.filelist[0])
        return self._chunk_cache[(depot_id, chunk_id)]

    get_chunk._vsz_patched = True
    CDNClient.get_chunk = get_chunk


def token_login(client):
    """CM logon using the saved refresh token in place of a password."""
    from steam.core.msg import MsgProto
    from steam.enums import EResult
    from steam.enums.emsg import EMsg
    from steam.steamid import SteamID
    from steam.utils import ip4_to_int

    tok = json.loads(TOKEN_FILE.read_text())
    eresult = client._pre_login()
    if eresult != EResult.OK:
        return eresult
    client.username = tok["account_name"]
    m = MsgProto(EMsg.ClientLogon)
    m.header.steamid = SteamID(tok["steamid"])
    m.body.protocol_version = 65580
    m.body.client_package_version = 1561159470
    m.body.client_os_type = 20
    m.body.client_language = "english"
    m.body.supports_rate_limit_response = True
    m.body.obfuscated_private_ip.v4 = ip4_to_int(client.connection.local_address) ^ 0xF00DBAAD
    m.body.account_name = tok["account_name"]
    m.body.access_token = tok["refresh_token"]
    client.send(m)
    resp = client.wait_msg(EMsg.ClientLogOnResponse, timeout=30)
    if resp and resp.body.eresult == EResult.OK:
        client.sleep(0.5)
    return EResult(resp.body.eresult) if resp else EResult.Fail


def cmd_test():
    from steam.client import SteamClient
    from steam.client.cdn import CDNClient
    from steam.enums import EResult
    c = SteamClient()
    r = token_login(c)
    print("cm login:", repr(r))
    if r != EResult.OK:
        return 1
    cdn = CDNClient(c)
    gid = 2608820895600961883  # Melee Captain Falcon UGC
    code = cdn.get_manifest_request_code(383980, 383980, gid)
    m = cdn.get_manifest(383980, 383980, gid, manifest_request_code=code)
    print("manifest:", m)
    for f in list(m.iter_files())[:10]:
        print(" ", f.filename, f.size)
    return 0


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "login"
    sys.exit({"login": cmd_login, "test": cmd_test}[cmd]())
