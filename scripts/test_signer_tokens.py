import tempfile
import unittest
from pathlib import Path

import remote_onboard
import setup_doctor


class RemoteOnboardTests(unittest.TestCase):
    def test_primary_order_signer_token_falls_back_to_remote_signer_token(self) -> None:
        runtime_cfg = {"order_signer_token": ""}
        self.assertEqual(
            remote_onboard._resolve_order_signer_primary_token(runtime_cfg, "remote-token"),
            "remote-token",
        )

    def test_primary_order_signer_token_prefers_distinct_runtime_token(self) -> None:
        runtime_cfg = {"order_signer_token": "primary-token"}
        self.assertEqual(
            remote_onboard._resolve_order_signer_primary_token(runtime_cfg, "remote-token"),
            "primary-token",
        )


class SetupDoctorTests(unittest.TestCase):
    def test_doctor_user_facing_baseline_hides_primary_token(self) -> None:
        self.assertNotIn(
            "EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN",
            [key for key, _, _ in setup_doctor.BASELINE_GENERATED],
        )

    def test_doctor_backfills_internal_primary_token_from_remote_signer(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            env_path = Path(tmpdir) / ".env"
            env_path.write_text(
                "EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN=remote-token\n",
                encoding="utf-8",
            )

            self.assertTrue(setup_doctor._ensure_internal_primary_signer_token(env_path))
            self.assertEqual(
                remote_onboard._read_env_value(
                    str(env_path), "EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN"
                ),
                "remote-token",
            )
            self.assertFalse(setup_doctor._ensure_internal_primary_signer_token(env_path))

    def test_doctor_requests_refresh_when_internal_primary_token_drifts(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            env_path = Path(tmpdir) / ".env"
            env_path.write_text(
                "\n".join(
                    [
                        "EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN=remote-token",
                        "EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN=stale-token",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )

            self.assertTrue(setup_doctor._needs_internal_signer_refresh(env_path))


if __name__ == "__main__":
    unittest.main()
