# Flows endpoint test fixtures — TEST ONLY

Every key in this directory was generated with `openssl` solely for the unit
tests in `../tests.rs`. None of them is, or ever was, registered with Meta or
used anywhere else. Do not reuse them.

- `test_rsa2048_pkcs8.pem`, `test_rsa2048_pkcs1.pem`, `test_rsa2048_public.pem`:
  one 2048-bit RSA key in PKCS#8, PKCS#1 and SPKI form.
- `test_rsa2048_*_encrypted.pem`: the same key password-protected (password
  `kat`), to prove encrypted PEMs are rejected with instructions.
- `test_rsa1024_pkcs8.pem`, `test_ec_p256_pkcs8.pem`: keys WhatsApp Flows
  cannot use (wrong size, not RSA).
- `kat_node.json`: a known-answer vector produced by `kat_node.mjs` (run
  `node kat_node.mjs` in this directory). The script simulates the WhatsApp
  client's request encryption, then runs the `decryptRequest` /
  `encryptResponse` functions from the Node.js example in Meta's
  `flows/guides/implementingyourflowendpoint` to confirm the request
  decrypts and to compute the expected sealed responses. The vector was also
  cross-checked against the Python example on the same page. RSA-OAEP is
  randomized, so re-running the script yields a different, equally valid
  vector.
- `kat_media.json`: produced by `kat_media.py` (Python `cryptography`),
  following the steps in `flows/guides/media_upload`. Meta publishes no
  media vector; the HMAC input order (IV, then ciphertext) is our reading of
  those steps, so this vector pins the code to that reading rather than
  proving it against Meta.
