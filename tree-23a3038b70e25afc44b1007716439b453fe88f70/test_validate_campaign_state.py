"""Focused adversarial checks for the coordination evidence boundary."""

import copy
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "validate_campaign_state", ROOT / "scripts" / "validate-campaign-state.py",
)
VALIDATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VALIDATOR)


class CampaignStateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.documents, cls.schemas = VALIDATOR.load_bundle(ROOT)

    def setUp(self):
        self.data = copy.deepcopy(self.documents)
        self.schema = copy.deepcopy(self.schemas)

    def errors(self):
        return VALIDATOR.validate(self.data, self.schema, ROOT)

    def reject(self, message):
        self.assertTrue(any(message in error for error in self.errors()), message)

    def test_current_blocked_snapshot_is_valid(self):
        self.assertEqual(self.errors(), [])

    def test_unknown_status_rejected(self):
        self.data["campaign-state"]["gates"][0]["status"] = "complete"
        self.reject("violates enum")

    def test_unknown_fields_rejected(self):
        self.data["campaign-state"]["ready"] = True
        self.reject("violates additionalProperties")

    def test_deployment_authority_cannot_be_enabled(self):
        self.data["campaign-state"]["operational_authorization"] = True
        self.reject("violates const")

    def test_preview_promotion_cannot_be_enabled(self):
        self.data["preview-lock"]["promotion_authorized"] = True
        self.reject("violates const")

    def test_source_hash_drift_rejected(self):
        self.data["evidence-index"]["sources"][0]["sha256"] = "0" * 64
        self.reject("source digest changed")

    def test_source_outside_checkout_rejected(self):
        with tempfile.NamedTemporaryFile(suffix=".md") as external:
            self.data["evidence-index"]["sources"][0]["path"] = os.path.relpath(external.name, ROOT)
            self.reject("path escapes repository")

    def test_source_line_range_checked(self):
        self.data["evidence-index"]["evidence"][0]["line_end"] = 999999
        self.reject("invalid source line range")

    def test_duplicate_evidence_identity_rejected(self):
        self.data["evidence-index"]["evidence"].append(
            copy.deepcopy(self.data["evidence-index"]["evidence"][0]),
        )
        self.reject("evidence: duplicate id")

    def test_missing_evidence_reference_rejected(self):
        self.data["hosts"]["hosts"][1]["facts"][0]["evidence_refs"] = ["E-MISSING"]
        self.reject("unknown evidence reference")

    def test_historical_fact_cannot_be_relabelled_observed(self):
        self.data["hosts"]["hosts"][0]["facts"][0]["status"] = "observed"
        self.reject("cannot be promoted or relabeled")

    def test_historical_source_cannot_be_relabelled_observed(self):
        item = next(x for x in self.data["evidence-index"]["evidence"]
                    if x["id"] == "E-LEDGER-GATES")
        item["status"] = "observed"
        item["kind"] = "observation"
        self.reject("evidence/source status mismatch")

    def test_observed_snapshot_is_not_gate_qualification(self):
        gate = self.data["campaign-state"]["gates"][0]
        gate["status"] = "observed"
        gate["evidence_refs"] = ["E-HOST"]
        self.reject("matching qualification evidence")

    def test_gate_requires_independent_named_verifier(self):
        gate = self.data["campaign-state"]["gates"][0]
        gate.update(status="observed", evidence_refs=["E-HOST"], author="author", verifier="author")
        self.reject("separate actual author and verifier")

    def test_gate_sequence_cannot_be_skipped(self):
        self.data["campaign-state"]["gates"][6]["depends_on"] = ["G0"]
        self.reject("invalid gate dependencies")

    def test_resume_cannot_jump_past_unverified_gate(self):
        self.data["campaign-state"]["next_gate"] = "G6"
        self.reject("earliest unverified gate")

    def test_blocker_links_are_bidirectional(self):
        self.data["campaign-state"]["gates"][0]["blocker_ids"].remove("B-FRESH")
        self.reject("missing reciprocal gate link")

    def test_consumer_order_cannot_change(self):
        repos = self.data["repository-rollout"]["repositories"]
        repos[0], repos[1] = repos[1], repos[0]
        self.reject("incorrect consumer order")

    def test_historical_runs_do_not_qualify_a_consumer(self):
        record = self.data["repository-rollout"]["repositories"][0]["qualification"]["local-mac"]
        record.update(status="observed", evidence_refs=["E-LEDGER-RUNS"])
        self.reject("cannot be promoted or relabeled")

    def test_bastion_cannot_qualify_before_mac(self):
        record = self.data["repository-rollout"]["repositories"][0]["qualification"]["bastion"]
        record.update(status="observed", evidence_refs=["E-BASTION-REFRESH"])
        self.reject("incomplete Mac rollout")

    def test_stale_source_prefix_rejected_in_every_component(self):
        for index, component in enumerate(self.documents["preview-lock"]["components"]):
            with self.subTest(component=component["id"]):
                self.data = copy.deepcopy(self.documents)
                self.data["preview-lock"]["components"][index]["source_commit"] = "f7bc191" + "0" * 33
                self.reject("forbidden stale source")

    def test_stale_exclusion_cannot_be_removed(self):
        self.data["preview-lock"]["excluded_source_prefixes"] = ["abcdef0"]
        self.reject("violates contains")

    def test_in_progress_preview_cannot_be_resolved(self):
        self.data["preview-lock"]["status"] = "observed"
        self.reject("successful completed run and attempt")

    def test_unresolved_preview_has_no_lock_id(self):
        self.data["preview-lock"]["lock_id"] = "a" * 64
        self.reject("unresolved lock must not have a lock identity")

    def test_running_preview_cannot_claim_success(self):
        self.data["preview-lock"]["observed_inputs"]["preview_run"]["conclusion"] = "success"
        self.reject("run state and conclusion disagree")

    def test_archived_component_proof_rejected(self):
        component = self.data["preview-lock"]["components"][0]
        component.update(status="observed", evidence_refs=["E-LEDGER-GATES"])
        self.reject("cannot be promoted or relabeled")

    def test_mismatched_record_times_rejected(self):
        self.data["hosts"]["recorded_at"] = "2026-09-22T23:52:15Z"
        self.reject("inconsistent recorded_at")

    def test_invalid_calendar_timestamp_rejected(self):
        self.data["campaign-state"]["recorded_at"] = "2026-02-30T00:00:00Z"
        self.reject("violates format")

    def test_date_only_observation_requires_timezone(self):
        self.data["evidence-index"]["evidence"][-1]["timezone"] = None
        self.reject("date requires timezone")

    def test_duplicate_json_keys_rejected(self):
        with patch.object(Path, "read_text", return_value='{"status":"blocked","status":"observed"}'):
            with self.assertRaisesRegex(ValueError, "duplicate JSON object key"):
                VALIDATOR.read_json(Path("unused"))

    def test_nonfinite_json_numbers_rejected(self):
        for constant in ("NaN", "Infinity", "-Infinity"):
            with self.subTest(constant=constant):
                with patch.object(Path, "read_text", return_value='{"n":' + constant + '}'):
                    with self.assertRaisesRegex(ValueError, "non-finite JSON number"):
                        VALIDATOR.read_json(Path("unused"))

    def test_token_signatures_rejected_without_echo(self):
        synthetic = "github_pat_" + "X" * 40
        self.data["preview-lock"]["reason"] = synthetic
        errors = self.errors()
        self.assertTrue(any("possible secret" in error for error in errors))
        self.assertNotIn(synthetic, "\n".join(errors))

    def test_credential_url_rejected_without_echo(self):
        synthetic = "https://operator:synthetic-password@example.invalid"
        self.data["preview-lock"]["reason"] = synthetic
        errors = self.errors()
        self.assertTrue(any("possible secret" in error for error in errors))
        self.assertNotIn(synthetic, "\n".join(errors))

    def test_remote_schema_resolution_is_refused(self):
        self.schema["campaign-state"]["properties"]["recorded_at"] = {
            "$ref": "https://unavailable.invalid/schema.json",
        }
        with patch("socket.create_connection", side_effect=AssertionError("network forbidden")):
            self.reject("unresolved or invalid local schema reference")

    def test_scalar_schema_rejected_without_traceback(self):
        self.schema["common"] = None
        self.reject("unexpected schema identity")


if __name__ == "__main__":
    unittest.main()
