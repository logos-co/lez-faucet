# Screenshots

`before-v0.2-wallet-password.png` — the shipped v0.2 first screen. It asks for a
"Wallet password (not encryption)" and states that LEZ v0.2.0 stores key
material in a plaintext file. The password is accepted by the pinned wallet API
and then ignored: it encrypts nothing and takes no part in key derivation.

`after-v0.3-one-field-receipt.png` — the same app after this change. One address
field, one button, and a receipt that reports the observed before/after balance
and the transaction hash. No wallet, no password, no persistence.

Both are cropped to the application window. Neither contains an account the
project does not control, a secret, or any third-party content.
