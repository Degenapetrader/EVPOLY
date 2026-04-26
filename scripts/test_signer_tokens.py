import unittest

import remote_onboard
import setup_doctor


class RemoteOnboardTests(unittest.TestCase):
    def test_relayer_submit_signer_url_uses_default(self) -> None:
        runtime_cfg = remote_onboard._resolve_runtime_config({"runtime": {}})
        self.assertEqual(
            runtime_cfg["relayer_submit_signer_url"],
            remote_onboard.DEFAULT_RELAYER_SUBMIT_SIGNER_URL,
        )

    def test_relayer_submit_signer_url_prefers_runtime_value(self) -> None:
        runtime_cfg = remote_onboard._resolve_runtime_config(
            {"runtime": {"relayer_submit_signer_url": "https://example.test/sign/submit"}}
        )
        self.assertEqual(
            runtime_cfg["relayer_submit_signer_url"],
            "https://example.test/sign/submit",
        )

    def test_relayer_submit_signer_url_accepts_legacy_submit_alias(self) -> None:
        runtime_cfg = remote_onboard._resolve_runtime_config(
            {"runtime": {"submit_signer_url": "https://example.test/legacy-submit"}}
        )
        self.assertEqual(
            runtime_cfg["relayer_submit_signer_url"],
            "https://example.test/legacy-submit",
        )


class SetupDoctorTests(unittest.TestCase):
    def test_doctor_user_facing_baseline_has_no_order_posting_signer_token(self) -> None:
        generated_keys = [key for key, _, _ in setup_doctor.BASELINE_GENERATED]
        self.assertIn("EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN", generated_keys)
        self.assertTrue(all("ORDER_SIGNER" not in key for key in generated_keys))
        self.assertTrue(all("BUILDER_REMOTE" not in key for key in generated_keys))


if __name__ == "__main__":
    unittest.main()
