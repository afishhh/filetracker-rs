#!/usr/bin/env python3
import string
import sys
from time import sleep
import hashlib
import requests

def is_sha256_hash(text: str) -> bool:
    HASH_ALPHABET = string.digits + string.ascii_lowercase

    if len(text) != 64:
        return False

    for chr in text:
        if chr not in HASH_ALPHABET:
            return False

    return True

filetracker = sys.argv[1]
remote_path = sys.argv[2]
local_path = sys.argv[3]

assert remote_path.startswith("/")

# wait for filetracker to come online
tries = 0
while True:
    try:
        requests.get(f"{filetracker}/version").raise_for_status()
        break
    except requests.ConnectionError as error:
        print(f"Failed to connect to filetracker: {error}")
        tries += 1

        if tries < 3:
            print("Retrying in 3 seconds...")
            sleep(3)
        else:
            raise error

head_response = requests.head(f"{filetracker}/files{remote_path}")
if head_response.status_code == 404:
    upload = True
else:
    head_response.raise_for_status()
    local_hash = hashlib.sha256(open(local_path, 'rb').read(), usedforsecurity=False).hexdigest()
    remote_hash = head_response.headers["SHA256-Checksum"]

    assert is_sha256_hash(remote_hash)
    assert is_sha256_hash(local_hash)

    print(f"remote hash: {remote_hash}")
    print(f"local hash: {local_hash}")

    upload = remote_hash != local_hash

if upload:
    print("Uploading file to filetracker")
    requests.put(
        f"{filetracker}/files{remote_path}",
        data=open(local_path, 'rb')
    ).raise_for_status()
else:
    print("Skipping file upload")
