#!/usr/bin/env python3
"""Small, deterministic lifecycle workloads used by the acceptance harness.

The workloads deliberately run without network, containers, a database, or a
model download.  Each returns a measured result and persists its inputs and
outputs so that a caller can inspect the hash-addressed evidence afterwards.
"""

from __future__ import annotations

import argparse
import hashlib
import html
import json
import math
import os
import platform
import random
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable


WorkloadResult = dict[str, Any]


def canonical(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def digest(value: Any) -> str:
    return "sha256:" + hashlib.sha256(canonical(value)).hexdigest()


def file_digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("wb", dir=path.parent, delete=False) as handle:
        handle.write(canonical(value))
        handle.flush()
        os.fsync(handle.fileno())
        temporary = Path(handle.name)
    os.replace(temporary, path)


def sigmoid(value: float) -> float:
    if value >= 0:
        return 1.0 / (1.0 + math.exp(-value))
    exp_value = math.exp(value)
    return exp_value / (1.0 + exp_value)


def logit(probability: float) -> float:
    probability = min(max(probability, 1e-6), 1.0 - 1e-6)
    return math.log(probability / (1.0 - probability))


def scalar(value: float | int) -> float | int:
    return round(value, 6) if isinstance(value, float) else value


def frontend_escape_cards() -> WorkloadResult:
    cards = [
        {"title": "API contract", "status": "official"},
        {"title": "<script>alert(1)</script>", "status": "proposed"},
        {"title": "Evidence", "status": "official"},
    ]
    rendered = "".join(
        f'<article data-status="{html.escape(card["status"], quote=True)}">'
        f"<h2>{html.escape(card['title'])}</h2></article>"
        for card in cards
    )
    assert "<script>" not in rendered
    assert "&lt;script&gt;" in rendered
    return {
        "metric": "escaped_card_count",
        "value": len(cards),
        "unit": "cards",
        "method": "html.escape render assertion",
        "note": "Escapes untrusted card titles before rendering.",
        "evidence": {"rendered": rendered},
    }


def frontend_role_filter() -> WorkloadResult:
    rows = [
        {"id": "rev_fe", "roles": ["fe", "review"]},
        {"id": "rev_be", "roles": ["be"]},
        {"id": "rev_ml", "roles": ["ml", "review"]},
    ]
    selected_role = "review"
    visible = [row["id"] for row in rows if selected_role in row["roles"]]
    assert visible == ["rev_fe", "rev_ml"]
    return {
        "metric": "visible_role_records",
        "value": len(visible),
        "unit": "records",
        "method": "role membership filter",
        "note": "Role filter preserves deterministic source order.",
        "evidence": {"role": selected_role, "visible": visible},
    }


def frontend_timeline_serialization() -> WorkloadResult:
    entries = [
        {"id": "hc_2", "at": "2026-09-07T01:00:00+00:00"},
        {"id": "hc_1", "at": "2026-09-07T00:00:00+00:00"},
        {"id": "pub_1", "at": "2026-09-07T02:00:00+00:00"},
    ]
    ordered = sorted(entries, key=lambda row: datetime.fromisoformat(row["at"]))
    payload = json.loads(json.dumps(ordered, ensure_ascii=False))
    assert [row["id"] for row in payload] == ["hc_1", "hc_2", "pub_1"]
    return {
        "metric": "serialized_timeline_events",
        "value": len(payload),
        "unit": "events",
        "method": "RFC3339 sort and JSON round-trip",
        "note": "Timeline order is computed from timestamps, not insertion order.",
        "evidence": {"timeline": payload},
    }


def backend_input_validation() -> WorkloadResult:
    def validate(payload: Any) -> bool:
        if not isinstance(payload, dict) or set(payload) != {"id", "title"}:
            return False
        return isinstance(payload["id"], str) and payload["id"].startswith("rec_") and isinstance(payload["title"], str)

    valid = {"id": "rec_1", "title": "Work item"}
    invalid = {"id": "rec_1", "title": "Work item", "unknown": True}
    assert validate(valid)
    assert not validate(invalid)
    return {
        "metric": "rejected_invalid_payloads",
        "value": 1,
        "unit": "payloads",
        "method": "strict allow-list request validation",
        "note": "Simulated service validation only; it makes no claim about a live idea_db endpoint.",
        "evidence": {"valid": valid, "invalid": invalid},
    }


def backend_idempotency() -> WorkloadResult:
    requests: dict[str, str] = {}

    def apply(key: str, body: dict[str, Any]) -> str:
        body_digest = digest(body)
        previous = requests.get(key)
        if previous is None:
            requests[key] = body_digest
            return "created"
        if previous == body_digest:
            return "replayed"
        return "conflict"

    body = {"operation": "capture", "content": "same event"}
    outcomes = [apply("key-1", body), apply("key-1", body), apply("key-1", {"operation": "capture", "content": "changed"})]
    assert outcomes == ["created", "replayed", "conflict"]
    return {
        "metric": "idempotency_replays",
        "value": outcomes.count("replayed"),
        "unit": "replays",
        "method": "canonical request digest cache",
        "note": "In-memory unit workload; it validates the idempotency state transition only.",
        "evidence": {"outcomes": outcomes},
    }


def backend_cas() -> WorkloadResult:
    head = {"revision": "rev_1"}

    def compare_and_set(expected: str, replacement: str) -> bool:
        if head["revision"] != expected:
            return False
        head["revision"] = replacement
        return True

    first = compare_and_set("rev_1", "rev_2")
    stale = compare_and_set("rev_1", "rev_3")
    assert first and not stale and head["revision"] == "rev_2"
    return {
        "metric": "stale_cas_rejections",
        "value": int(not stale),
        "unit": "requests",
        "method": "compare expected head before replacement",
        "note": "Single-process simulation of CAS semantics, not a database concurrency claim.",
        "evidence": {"first_applied": first, "stale_applied": stale, "head": head},
    }


def infrastructure_atomic_persistence() -> WorkloadResult:
    with tempfile.TemporaryDirectory() as directory:
        artifact = Path(directory) / "state.json"
        expected = {"seq": 7, "records": ["rev_a", "rev_b"]}
        atomic_json(artifact, expected)
        loaded = json.loads(artifact.read_text("utf-8"))
        assert loaded == expected
        artifact_hash = file_digest(artifact)
    return {
        "metric": "atomic_round_trips",
        "value": 1,
        "unit": "files",
        "method": "fsync temporary file then os.replace",
        "note": "Writes and verifies an isolated temporary artifact.",
        "evidence": {"temporary_artifact_sha256": artifact_hash},
    }


def infrastructure_secret_free_config() -> WorkloadResult:
    config = {
        "bind": "127.0.0.1:8080",
        "static_dir": "./static",
        "neo4j_uri": "http://127.0.0.1:7474",
        "token": None,
    }
    forbidden = {"password", "secret", "api_key"}
    present = sorted(key for key, value in config.items() if key in forbidden and value)
    assert not present
    return {
        "metric": "persisted_secret_values",
        "value": len(present),
        "unit": "values",
        "method": "non-empty forbidden configuration key scan",
        "note": "Uses a non-secret fixture; no credential is read or persisted.",
        "evidence": {"config_digest": digest(config), "forbidden_present": present},
    }


def infrastructure_restart_manifest() -> WorkloadResult:
    manifest = {"binary": "idea-db", "static_dir": "./static", "resume_seq": 18}
    encoded = canonical(manifest)
    recovered = json.loads(encoded.decode("utf-8"))
    assert recovered == manifest
    return {
        "metric": "restart_manifest_fields",
        "value": len(recovered),
        "unit": "fields",
        "method": "canonical manifest serialize and recovery parse",
        "note": "Manifest-only recovery check; it does not start a process.",
        "evidence": {"manifest": recovered, "manifest_sha256": digest(manifest)},
    }


def linear_data(seed: int = 17) -> tuple[list[tuple[float, float]], list[int], list[tuple[float, float]], list[int]]:
    rng = random.Random(seed)
    points: list[tuple[float, float]] = []
    labels: list[int] = []
    for _ in range(180):
        first = rng.uniform(-1.0, 1.0)
        second = rng.uniform(-1.0, 1.0)
        points.append((first, second))
        labels.append(int(first + 0.7 * second + rng.uniform(-0.18, 0.18) > 0))
    return points[:130], labels[:130], points[130:], labels[130:]


def train_logistic(features: list[tuple[float, float]], labels: list[int], epochs: int = 180) -> tuple[float, float, float]:
    weight_one, weight_two, bias = 0.0, 0.0, 0.0
    for _ in range(epochs):
        gradient_one = gradient_two = gradient_bias = 0.0
        for (first, second), label in zip(features, labels):
            error = sigmoid(weight_one * first + weight_two * second + bias) - label
            gradient_one += error * first
            gradient_two += error * second
            gradient_bias += error
        rate = 0.55 / len(features)
        weight_one -= rate * gradient_one
        weight_two -= rate * gradient_two
        bias -= rate * gradient_bias
    return weight_one, weight_two, bias


def linear_probabilities(model: tuple[float, float, float], features: list[tuple[float, float]]) -> list[float]:
    first_weight, second_weight, bias = model
    return [sigmoid(first_weight * first + second_weight * second + bias) for first, second in features]


def machine_learning_train_holdout() -> WorkloadResult:
    train_x, train_y, holdout_x, holdout_y = linear_data()
    model = train_logistic(train_x, train_y)
    probabilities = linear_probabilities(model, holdout_x)
    accuracy = sum(int((probability >= 0.5) == bool(label)) for probability, label in zip(probabilities, holdout_y)) / len(holdout_y)
    assert accuracy >= 0.8
    return {
        "metric": "holdout_accuracy",
        "value": scalar(accuracy),
        "unit": "fraction",
        "method": "deterministic logistic regression gradient descent",
        "note": "Synthetic deterministic train/holdout split; no production model is claimed.",
        "evidence": {"model": [scalar(weight) for weight in model], "holdout_size": len(holdout_y)},
    }


def machine_learning_drift() -> WorkloadResult:
    train_x, _, holdout_x, _ = linear_data()
    train_mean = sum(point[0] for point in train_x) / len(train_x)
    shifted = [(point[0] + 0.45, point[1]) for point in holdout_x]
    shifted_mean = sum(point[0] for point in shifted) / len(shifted)
    drift = abs(shifted_mean - train_mean)
    assert drift > 0.3
    return {
        "metric": "feature_mean_shift",
        "value": scalar(drift),
        "unit": "feature_units",
        "method": "train versus shifted holdout mean comparison",
        "note": "Measured deterministic synthetic covariate shift.",
        "evidence": {"train_mean": scalar(train_mean), "shifted_mean": scalar(shifted_mean)},
    }


def machine_learning_recalibration() -> WorkloadResult:
    train_x, train_y, holdout_x, holdout_y = linear_data(seed=17)
    calibration_x, calibration_y = holdout_x[:25], holdout_y[:25]
    test_x, test_y = holdout_x[25:], holdout_y[25:]
    model = train_logistic(train_x, train_y)

    def brier(probabilities: list[float], labels: list[int]) -> float:
        return sum((probability - label) ** 2 for probability, label in zip(probabilities, labels)) / len(labels)

    calibration_overconfident = [
        sigmoid(1.8 * logit(probability))
        for probability in linear_probabilities(model, calibration_x)
    ]
    candidates = [0.7 + step * 0.1 for step in range(18)]
    calibration_scores = [
        (
            candidate,
            brier(
                [sigmoid(logit(probability) / candidate) for probability in calibration_overconfident],
                calibration_y,
            ),
        )
        for candidate in candidates
    ]
    temperature, calibration_chosen_score = min(
        calibration_scores,
        key=lambda score: (score[1], score[0]),
    )
    calibration_grid = [
        {"temperature": scalar(candidate), "brier_score": scalar(score)}
        for candidate, score in calibration_scores
    ]
    test_overconfident = [
        sigmoid(1.8 * logit(probability))
        for probability in linear_probabilities(model, test_x)
    ]
    test_before = brier(test_overconfident, test_y)
    test_after = brier(
        [sigmoid(logit(probability) / temperature) for probability in test_overconfident],
        test_y,
    )
    return {
        "metric": "untouched_test_recalibrated_brier_score",
        "value": scalar(test_after),
        "unit": "mean_squared_probability_error",
        "method": "temperature selected by calibration Brier score and evaluated once on disjoint test data",
        "note": "Untouched-test calibration outcome is recorded as observed; improvement is not assumed.",
        "evidence": {
            "dataset_seed": 17,
            "split_indices": {
                "train": [0, 130],
                "calibration": [130, 155],
                "test": [155, 180],
            },
            "model": [scalar(weight) for weight in model],
            "overconfidence_logit_scale": 1.8,
            "chosen_temperature": scalar(temperature),
            "calibration_grid": calibration_grid,
            "calibration_chosen_brier_score": scalar(calibration_chosen_score),
            "test_before": scalar(test_before),
            "test_after": scalar(test_after),
            "test_delta": scalar(test_after - test_before),
        },
    }


def xor_data(seed: int = 31) -> tuple[list[tuple[float, float]], list[int], list[tuple[float, float]], list[int]]:
    rng = random.Random(seed)
    points: list[tuple[float, float]] = []
    labels: list[int] = []
    for _ in range(240):
        first = rng.uniform(-1.0, 1.0)
        second = rng.uniform(-1.0, 1.0)
        points.append((first, second))
        labels.append(int((first > 0) != (second > 0)))
    return points[:180], labels[:180], points[180:], labels[180:]


def train_mlp(features: list[tuple[float, float]], labels: list[int], seed: int = 5) -> tuple[list[list[float]], list[float], list[float], float]:
    rng = random.Random(seed)
    hidden_weights = [[rng.uniform(-0.5, 0.5), rng.uniform(-0.5, 0.5)] for _ in range(4)]
    hidden_biases = [0.0] * 4
    output_weights = [rng.uniform(-0.5, 0.5) for _ in range(4)]
    output_bias = 0.0
    for _ in range(700):
        for (first, second), label in zip(features, labels):
            hidden = [sigmoid(weights[0] * first + weights[1] * second + bias) for weights, bias in zip(hidden_weights, hidden_biases)]
            probability = sigmoid(sum(weight * value for weight, value in zip(output_weights, hidden)) + output_bias)
            output_error = probability - label
            old_output = output_weights[:]
            for index in range(4):
                output_weights[index] -= 0.18 * output_error * hidden[index]
                hidden_error = output_error * old_output[index] * hidden[index] * (1.0 - hidden[index])
                hidden_weights[index][0] -= 0.18 * hidden_error * first
                hidden_weights[index][1] -= 0.18 * hidden_error * second
                hidden_biases[index] -= 0.18 * hidden_error
            output_bias -= 0.18 * output_error
    return hidden_weights, hidden_biases, output_weights, output_bias


def mlp_probabilities(model: tuple[list[list[float]], list[float], list[float], float], features: list[tuple[float, float]]) -> list[float]:
    hidden_weights, hidden_biases, output_weights, output_bias = model
    output = []
    for first, second in features:
        hidden = [sigmoid(weights[0] * first + weights[1] * second + bias) for weights, bias in zip(hidden_weights, hidden_biases)]
        output.append(sigmoid(sum(weight * value for weight, value in zip(output_weights, hidden)) + output_bias))
    return output


def deep_learning_holdout() -> WorkloadResult:
    train_x, train_y, holdout_x, holdout_y = xor_data()
    model = train_mlp(train_x, train_y)
    probabilities = mlp_probabilities(model, holdout_x)
    accuracy = sum(int((probability >= 0.5) == bool(label)) for probability, label in zip(probabilities, holdout_y)) / len(holdout_y)
    assert accuracy >= 0.85
    return {
        "metric": "xor_holdout_accuracy",
        "value": scalar(accuracy),
        "unit": "fraction",
        "method": "fixed-seed two-layer MLP backpropagation on CPU",
        "note": "Tiny deterministic MLP workload; no downloaded or live deep-learning model is used.",
        "evidence": {"hidden_units": 4, "holdout_size": len(holdout_y)},
    }


def deep_learning_bad_input() -> WorkloadResult:
    observed = None
    try:
        if not all(math.isfinite(value) for value in (0.2, float("nan"))):
            raise ValueError("non-finite feature")
    except ValueError as error:
        observed = str(error)
    assert observed == "non-finite feature"
    return {
        "status": "observed_failure",
        "metric": "non_finite_inputs_rejected",
        "value": 1,
        "unit": "inputs",
        "method": "finite-value input guard",
        "note": "Expected invalid-input failure was observed; this is not a failed workload assertion.",
        "evidence": {"error": observed},
    }


def deep_learning_changed_input() -> WorkloadResult:
    train_x, train_y, holdout_x, holdout_y = xor_data()
    model = train_mlp(train_x, train_y)
    normal = mlp_probabilities(model, holdout_x)
    changed = mlp_probabilities(model, [(first + 1.2, second - 1.2) for first, second in holdout_x])
    normal_accuracy = sum(int((value >= 0.5) == bool(label)) for value, label in zip(normal, holdout_y)) / len(holdout_y)
    changed_accuracy = sum(int((value >= 0.5) == bool(label)) for value, label in zip(changed, holdout_y)) / len(holdout_y)
    degradation = normal_accuracy - changed_accuracy
    assert degradation > 0.1
    return {
        "metric": "changed_input_accuracy_degradation",
        "value": scalar(degradation),
        "unit": "fraction",
        "method": "holdout evaluation after deterministic feature shift",
        "note": "Observed performance change is measured against the same trained fixed-seed model.",
        "evidence": {"normal_accuracy": scalar(normal_accuracy), "changed_accuracy": scalar(changed_accuracy)},
    }


def llm_prompt_revision() -> WorkloadResult:
    first = "Summarize this plan."
    revised = "Return JSON with claim, evidence_ids, and uncertainty. Cite only supplied evidence IDs."
    assert "evidence_ids" in revised and revised != first
    return {
        "metric": "prompt_instruction_words",
        "value": len(revised.split()),
        "unit": "words",
        "method": "deterministic prompt revision fixture",
        "note": "Fixture-only LLM adapter: no provider call or live inference occurred.",
        "evidence": {"before": first, "after": revised},
    }


def llm_structured_response() -> WorkloadResult:
    def validate(response: Any) -> bool:
        return (
            isinstance(response, dict)
            and set(response) == {"claim", "evidence_ids", "uncertainty"}
            and isinstance(response["claim"], str)
            and isinstance(response["evidence_ids"], list)
            and all(isinstance(item, str) for item in response["evidence_ids"])
            and isinstance(response["uncertainty"], float)
            and 0.0 <= response["uncertainty"] <= 1.0
        )

    accepted = {"claim": "Cache invalidation needs a test.", "evidence_ids": ["cap_1"], "uncertainty": 0.2}
    rejected = {"claim": "Bad type", "evidence_ids": "cap_1", "uncertainty": 2.0}
    assert validate(accepted) and not validate(rejected)
    return {
        "metric": "valid_structured_responses",
        "value": 1,
        "unit": "fixtures",
        "method": "strict deterministic response-schema validation",
        "note": "Fixture-only LLM adapter validates a response shape without inference.",
        "evidence": {"accepted": accepted, "rejected": rejected},
    }


def llm_citation_guard() -> WorkloadResult:
    known_evidence = {"cap_1": "A source sentence", "cap_2": "Another source sentence"}
    response = {"claim": "A source sentence", "evidence_ids": ["cap_1"]}
    invalid_response = {"claim": "Unsupported", "evidence_ids": ["cap_missing"]}

    def guard(candidate: dict[str, Any]) -> bool:
        return all(evidence_id in known_evidence for evidence_id in candidate.get("evidence_ids", []))

    assert guard(response)
    assert not guard(invalid_response)
    return {
        "status": "observed_failure",
        "metric": "unsupported_citations_blocked",
        "value": 1,
        "unit": "responses",
        "method": "evidence identifier allow-list guard",
        "note": "Expected unsupported-citation failure was observed in the fixture-only LLM adapter.",
        "evidence": {"accepted": response, "rejected": invalid_response},
    }


SPECS: tuple[tuple[str, str, str, Callable[[], WorkloadResult]], ...] = (
    ("fe_1", "fe", "escaped plan cards", frontend_escape_cards),
    ("fe_2", "fe", "role-filtered explorer", frontend_role_filter),
    ("fe_3", "fe", "timeline serialization", frontend_timeline_serialization),
    ("be_1", "be", "strict input validation", backend_input_validation),
    ("be_2", "be", "idempotency replay", backend_idempotency),
    ("be_3", "be", "head compare-and-set", backend_cas),
    ("infra_1", "infra", "atomic artifact persistence", infrastructure_atomic_persistence),
    ("infra_2", "infra", "secret-free configuration", infrastructure_secret_free_config),
    ("infra_3", "infra", "restart manifest recovery", infrastructure_restart_manifest),
    ("ml_1", "ml", "logistic train and holdout", machine_learning_train_holdout),
    ("ml_2", "ml", "covariate drift measurement", machine_learning_drift),
    ("ml_3", "ml", "probability recalibration", machine_learning_recalibration),
    ("dl_1", "dl", "two-layer MLP holdout", deep_learning_holdout),
    ("dl_2", "dl", "non-finite input guard", deep_learning_bad_input),
    ("dl_3", "dl", "changed-input evaluation", deep_learning_changed_input),
    ("llm_1", "llm", "prompt revision", llm_prompt_revision),
    ("llm_2", "llm", "structured response validation", llm_structured_response),
    ("llm_3", "llm", "citation evidence guard", llm_citation_guard),
)


def environment_for(work_id: str, result: WorkloadResult) -> dict[str, str]:
    config = {"work_id": work_id, "method": result["method"]}
    return {
        "code_ref": file_digest(Path(__file__)),
        "data_version": "deterministic-fixtures-v1",
        "model_version": "fixture-only" if work_id.startswith("llm_") else "stdlib-local-v1",
        "config_digest": digest(config),
        "runtime": f"python-{platform.python_version()} {platform.system().lower()}",
    }


def run_workloads(output_dir: Path) -> dict[str, dict]:
    """Run all 18 local workloads and write one hash-addressed artifact each."""
    output_dir = Path(output_dir).resolve()
    results: dict[str, dict] = {}
    for work_id, role, name, runner in SPECS:
        result = runner()
        status = result.pop("status", "passed")
        artifact = output_dir / f"{work_id}.json"
        saved = {
            "id": work_id,
            "role": role,
            "name": name,
            "status": status,
            "metric": result["metric"],
            "value": result["value"],
            "unit": result["unit"],
            "method": result["method"],
            "note": result["note"],
            "evidence": result["evidence"],
            "environment": environment_for(work_id, result),
            "generated_at": datetime.now(timezone.utc).isoformat(timespec="milliseconds"),
        }
        atomic_json(artifact, saved)
        results[work_id] = {
            "role": role,
            "name": name,
            "status": status,
            "metric": result["metric"],
            "value": result["value"],
            "unit": result["unit"],
            "method": result["method"],
            "artifact_path": str(artifact),
            "artifact_sha256": file_digest(artifact),
            "environment": saved["environment"],
            "note": result["note"],
        }
    return results


def main() -> None:
    parser = argparse.ArgumentParser(description="run deterministic lifecycle validation workloads")
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(run_workloads(args.output_dir), ensure_ascii=False, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
