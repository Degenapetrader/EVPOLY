import tempfile
import unittest
from pathlib import Path

import setup_doctor


class LocalSetupTests(unittest.TestCase):
    def audit(self, wallet_fields):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "test.env"
            path.write_text("POLY_PRIVATE_KEY=" + "1" * 64 + "\n" + wallet_fields, encoding="utf-8")
            return setup_doctor._collect_audit(path)

    def test_eoa_does_not_need_hosted_credentials(self):
        audit = self.audit("POLY_SIGNATURE_TYPE=0\nEVPOLY_ALPHA_AUTO_ONBOARD=false\n")
        self.assertEqual(audit["blocking_missing_labels"], [])
        self.assertEqual(audit["manual_missing_labels"], [])

    def test_proxy_can_trade_without_optional_relayer_credentials(self):
        audit = self.audit("POLY_SIGNATURE_TYPE=1\nPOLY_PROXY_WALLET_ADDRESS=0x" + "2" * 40)
        self.assertEqual(audit["blocking_missing_labels"], [])
        self.assertEqual(audit["manual_missing_labels"], ["Relayer API Key", "Relayer API Key Address"])

    def test_deposit_mode_is_preserved_and_requires_its_funder(self):
        self.assertEqual(self.audit("POLY_SIGNATURE_TYPE=3\n")["blocking_missing_labels"], ["Deposit Wallet"])
        audit = self.audit("POLY_SIGNATURE_TYPE=3\nPOLY_DEPOSIT_WALLET_ADDRESS=0x" + "3" * 40)
        self.assertEqual(audit["manual_missing_labels"], [])


if __name__ == "__main__":
    unittest.main()
