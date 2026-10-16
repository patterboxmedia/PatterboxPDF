"""Audit bounded writer evidence without inferring normative completeness."""
import argparse
import hashlib
import json
from pathlib import Path


def audit(root, inventory):
    if inventory["schema"] != "pdf-library-requirements/v1":
        raise ValueError("unknown requirements schema")
    rows = inventory["topics"] + inventory["writer_contracts"] + inventory["normative_requirements"]
    ids = [row["id"] for row in rows]
    if len(ids) != len(set(ids)):
        raise ValueError("duplicate requirement ID")
    authority = json.loads((root / inventory["retained_topic_authority"]).read_text(encoding="utf-8"))
    if {row["id"] for row in inventory["topics"]} != {row["rule_id"] for row in authority["rules"]}:
        raise ValueError("retained topic inventory is incomplete")
    pending = []
    for row in rows:
        if row["status"] not in {"verified", "excluded", "not-in-library-scope", "pending-current-evidence", "normative-pending"}:
            raise ValueError("unknown requirement status")
        for implementation in row.get("implementation", []):
            path = (root / implementation["path"]).resolve(strict=True)
            if not path.is_relative_to(root):
                raise ValueError("implementation reference escapes workspace")
            if implementation.get("symbol") and implementation["symbol"] not in path.read_text(encoding="utf-8"):
                raise ValueError(f"missing implementation symbol: {implementation['symbol']}")
        evidence = row.get("evidence", [])
        for receipt in evidence:
            path = (root / receipt["path"]).resolve(strict=True)
            if not path.is_relative_to(root) or hashlib.sha256(path.read_bytes()).hexdigest() != receipt["sha256"]:
                raise ValueError("evidence identity or path drift")
        if row["status"] in {"verified", "excluded"} and (not evidence or not row.get("implementation")):
            raise ValueError("verified requirement lacks implementation or exact evidence")
        if row["status"] in {"pending-current-evidence", "normative-pending"}:
            pending.append(row["id"])
    complete = inventory["normative_pdfx4_coverage_complete"]
    assurance = inventory.get("coverage_assurance", {})
    if complete:
        if assurance.get("status") != "independently-reconciled" or not assurance.get("evidence_sha256"):
            raise ValueError("normative completeness lacks independent coverage evidence")
        normative_rows = inventory["normative_requirements"]
        if not normative_rows or any(not row.get("clause") or row["status"] not in {"verified", "excluded"} for row in normative_rows):
            raise ValueError("normative clauses remain unresolved")
        if pending:
            raise ValueError("normative completeness conflicts with unresolved inventory")
    return {"retained_topics": len(inventory["topics"]), "writer_contracts": len(inventory["writer_contracts"]),
            "normative_statements_reconciled": len(inventory["normative_requirements"]),
            "normative_pdfx4_coverage_complete": complete, "pending": pending}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory", type=Path, default=Path("docs/requirements/pdfx4-inventory.json"))
    parser.add_argument("--require-normative", action="store_true", help="Fail unless the full normative gate is closed")
    arguments = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    result = audit(root, json.loads(arguments.inventory.read_text(encoding="utf-8")))
    print(json.dumps(result, indent=2))
    if arguments.require_normative and not result["normative_pdfx4_coverage_complete"]:
        parser.exit(1, "Exhaustive PDF/X-4 coverage remains incomplete.\n")
