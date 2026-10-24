"""Uploads a build to temp.hysong.dev and prints its download link.

    python scripts/upload-temp.py <file> [name] [max downloads]

The site takes chunked uploads: create the upload, PUT each chunk with the upload token,
then complete it.
"""

import json
import mimetypes
import os
import sys
import urllib.request

BASE = "https://temp.hysong.dev"
TYPES = {".apk": "application/vnd.android.package-archive", ".msi": "application/x-msi"}

path = sys.argv[1]
name = sys.argv[2] if len(sys.argv) > 2 else os.path.basename(path)
limit = int(sys.argv[3]) if len(sys.argv) > 3 else 3
size = os.path.getsize(path)
ext = os.path.splitext(name)[1].lower()
mime = TYPES.get(ext) or mimetypes.guess_type(name)[0] or "application/octet-stream"
# The site turns away clients without a browser user agent.
UA = {"User-Agent": "Mozilla/5.0"}


def call(url, method, body=None, headers=None):
    req = urllib.request.Request(BASE + url, data=body, method=method, headers={**UA, **(headers or {})})
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.loads(r.read() or b"{}")


init = call(
    "/api/uploads",
    "POST",
    json.dumps({"filename": name, "size": size, "maxDownloads": limit, "mimeType": mime}).encode(),
    {"Content-Type": "application/json"},
)
token = {"X-Upload-Token": init["uploadToken"]}
with open(path, "rb") as f:
    for i in range(init["totalChunks"]):
        f.seek(i * init["chunkSize"])
        call(
            f"/api/uploads/{init['uploadId']}/chunks/{i}",
            "PUT",
            f.read(init["chunkSize"]),
            {**token, "Content-Type": "application/octet-stream"},
        )
done = call(f"/api/uploads/{init['uploadId']}/complete", "POST", b"", token)
print(done["url"])
