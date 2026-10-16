"""Prevent bounded evidence from being represented as full normative closure."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("requirements", Path(__file__).resolve().parents[2] / "scripts/verify-requirements.py")
requirements = importlib.util.module_from_spec(spec)
spec.loader.exec_module(requirements)


class RequirementsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "authority.json").write_text(json.dumps({"rules": [{"rule_id": "NORMATIVE"}]}))
        self.inventory = {"schema": "pdf-library-requirements/v1", "retained_topic_authority": "authority.json",
                          "coverage_assurance": {"status": "incomplete"},
                          "normative_pdfx4_coverage_complete": False, "normative_requirements": [],
                          "topics": [{"id": "NORMATIVE", "status": "normative-pending"}], "writer_contracts": []}

    def test_pending_inventory_is_explicitly_incomplete(self):
        result = requirements.audit(self.root, self.inventory)
        self.assertFalse(result["normative_pdfx4_coverage_complete"])
        self.assertEqual(result["pending"], ["NORMATIVE"])

    def test_rejects_unsupported_completeness_claim_and_duplicate_ids(self):
        claimed = copy.deepcopy(self.inventory)
        claimed["normative_pdfx4_coverage_complete"] = True
        with self.assertRaisesRegex(ValueError, "coverage evidence"):
            requirements.audit(self.root, claimed)
        duplicate = copy.deepcopy(self.inventory)
        duplicate["writer_contracts"] = [{"id": "NORMATIVE", "status": "normative-pending"}]
        with self.assertRaisesRegex(ValueError, "duplicate"):
            requirements.audit(self.root, duplicate)

    def test_verified_requirement_needs_exact_evidence(self):
        self.inventory["topics"][0]["status"] = "verified"
        with self.assertRaisesRegex(ValueError, "exact evidence"):
            requirements.audit(self.root, self.inventory)


if __name__ == "__main__":
    unittest.main()
