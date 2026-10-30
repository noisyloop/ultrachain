"""PDA derivation vectors cross-checked against solana-pubkey's
find_program_address. Run: python3 -m unittest discover -s bridge/scripts"""
import importlib.util
import pathlib
import unittest

_spec = importlib.util.spec_from_file_location(
    "check_supply", pathlib.Path(__file__).with_name("check-supply.py")
)
cs = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cs)

# program id -> (escrow PDA, mint PDA)
VECTORS = {
    "CKrNt2y1e2H9728mHRVYyF2owy9eKreepRKpTMj2j8JG": (
        "3BEJE6ZTg7Fu2wSVAoazRmxXpqT8284Zntozz7sVip9J",
        "GtXpaoJ7nZZjnSo3qzxyT7jP9Zu6LtXNVSVdD2ULyifL",
    ),
    "BGW8geBw12Yyp4rW7Fp97a3CQRA4Czi8PdtuV2Ec3Vdy": (
        "ApgfNGmLBtzDLm7wavGHGEuG4PPfFb1UWyH1jYe2RWG1",
        "qPzdrTCvxK3bxoh2YoTZtDcGVgRUwm37aQcC3abFgBy",
    ),
    "11111111111111111111111111111111": (
        "E1cyphGs2HNfnVhjS7mYaeeuCRJNCGcmyvVZphX6Sh9T",
        "5d8Pk7NzjsvLuHRrDhMUYcaMKs2rft1Pf41PFJcxywvb",
    ),
}


class PdaTest(unittest.TestCase):
    def test_matches_solana_sdk(self):
        for program, (escrow, mint) in VECTORS.items():
            self.assertEqual(cs.escrow_address(program), escrow)
            self.assertEqual(cs.mint_address(program), mint)

    def test_base58_roundtrip(self):
        for program in VECTORS:
            self.assertEqual(cs.b58encode(cs.b58decode(program)), program)
            self.assertEqual(len(cs.b58decode(program)), 32)


if __name__ == "__main__":
    unittest.main()
