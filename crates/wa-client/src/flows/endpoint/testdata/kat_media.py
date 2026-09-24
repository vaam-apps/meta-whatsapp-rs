"""Known-answer vector for Flow media decryption (flows/guides/media_upload).

Implements the documented *encryption* side independently of the Rust code
(Python `cryptography`): AES-256-CBC with PKCS#7, HMAC-SHA256 over
iv || ciphertext truncated to 10 bytes and appended, SHA-256 of the CDN file
and of the plaintext.

Meta publishes no media test vector, and its steps only say "calculate HMAC
with hmac_key, initialization vector and ciphertext": the iv-then-ciphertext
byte order is our reading of that list (and WhatsApp's usual media format),
not something this vector can prove. It pins the Rust code to that reading.

Run: python3 kat_media.py > kat_media.json
"""
import hashlib
import hmac
import json
import os
from base64 import b64encode

from cryptography.hazmat.primitives import padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

plaintext = b"\xff\xd8\xff\xe0 not really a JPEG, but it will do \xff\xd9"
enc_key, hmac_key, iv = os.urandom(32), os.urandom(32), os.urandom(16)
padder = padding.PKCS7(128).padder()
padded = padder.update(plaintext) + padder.finalize()
encryptor = Cipher(algorithms.AES(enc_key), modes.CBC(iv)).encryptor()
ciphertext = encryptor.update(padded) + encryptor.finalize()
hmac10 = hmac.new(hmac_key, iv + ciphertext, hashlib.sha256).digest()[:10]
cdn_file = ciphertext + hmac10


def b64(b):
    return b64encode(b).decode()


print(json.dumps({
    "cdn_file": b64(cdn_file),
    "plaintext": b64(plaintext),
    "media": {
        "media_id": "790aba14-5f4a-4dbd-aa9e-0d75401da14b",
        "cdn_url": "https://mmg.whatsapp.net/v/redacted",
        "file_name": "IMG_5237.jpg",
        "encryption_metadata": {
            "encrypted_hash": b64(hashlib.sha256(cdn_file).digest()),
            "iv": b64(iv),
            "encryption_key": b64(enc_key),
            "hmac_key": b64(hmac_key),
            "plaintext_hash": b64(hashlib.sha256(plaintext).digest()),
        },
    },
}, indent=2))
