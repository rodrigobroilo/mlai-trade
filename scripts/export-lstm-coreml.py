#!/usr/bin/env python3
"""Convert mlai-trade's portable LSTM0003 model to an ANE-oriented ML Program."""

import argparse
import hashlib
import json
import pathlib
import shutil
import struct
import subprocess

import coremltools as ct
import numpy as np
from coremltools.converters.mil import Builder as mb
from coremltools.converters.mil.mil import types


def read_exact(handle, length):
    value = handle.read(length)
    if len(value) != length:
        raise ValueError("truncated LSTM model")
    return value


def read_u32(handle):
    return struct.unpack("<I", read_exact(handle, 4))[0]


def read_f64(handle):
    return struct.unpack("<d", read_exact(handle, 8))[0]


def read_vector(handle):
    return np.asarray([read_f64(handle) for _ in range(read_u32(handle))], dtype=np.float16)


def load_model(path):
    with open(path, "rb") as handle:
        if read_exact(handle, 8) != b"LSTM0003":
            raise ValueError("Core ML export requires LSTM0003 model weights")
        input_dim = read_u32(handle)
        hidden_dim = read_u32(handle)
        sequence_length = read_u32(handle)
        target_mode = read_u32(handle)
        direction_threshold = read_f64(handle)
        target_mean = read_f64(handle)
        target_std = read_f64(handle)
        vectors = [read_vector(handle) for _ in range(9)]
        output_bias = read_f64(handle)
        if handle.read(1):
            raise ValueError("unexpected trailing LSTM model data")

    names = ["w_i", "b_i", "w_f", "b_f", "w_o", "b_o", "w_c", "b_c", "w_out"]
    model = dict(zip(names, vectors))
    gate_dim = input_dim + hidden_dim
    for name in ["w_i", "w_f", "w_o", "w_c"]:
        expected = hidden_dim * gate_dim
        if model[name].size != expected:
            raise ValueError(f"{name} has {model[name].size} values, expected {expected}")
        model[name] = model[name].reshape(hidden_dim, gate_dim)
    for name in ["b_i", "b_f", "b_o", "b_c", "w_out"]:
        if model[name].size != hidden_dim:
            raise ValueError(f"{name} has {model[name].size} values, expected {hidden_dim}")
    if target_mode not in (0, 1):
        raise ValueError(f"unsupported target mode {target_mode}")
    model.update(
        input_dim=input_dim,
        hidden_dim=hidden_dim,
        sequence_length=sequence_length,
        target_mode=target_mode,
        direction_threshold=direction_threshold,
        target_mean=target_mean,
        target_std=target_std,
        output_bias=np.float16(output_bias),
    )
    return model


def build_program(model, batch_size):
    input_dim = model["input_dim"]
    hidden_dim = model["hidden_dim"]
    sequence_length = model["sequence_length"]

    @mb.program(
        input_specs=[
            mb.TensorSpec(
                shape=(batch_size, sequence_length, input_dim),
                dtype=types.fp32,
            )
        ],
        opset_version=ct.target.macOS13,
    )
    def program(sequence):
        sequence_fp16 = mb.cast(x=sequence, dtype="fp16")
        hidden = np.zeros((batch_size, hidden_dim), dtype=np.float16)
        cell = np.zeros((batch_size, hidden_dim), dtype=np.float16)
        for step in range(sequence_length):
            features = mb.slice_by_index(
                x=sequence_fp16,
                begin=[0, step, 0],
                end=[batch_size, step + 1, input_dim],
                squeeze_mask=[False, True, False],
            )
            combined = mb.concat(values=[hidden, features], axis=1)
            input_gate = mb.sigmoid(
                x=mb.add(x=mb.matmul(x=combined, y=model["w_i"].T), y=model["b_i"])
            )
            forget_gate = mb.sigmoid(
                x=mb.add(x=mb.matmul(x=combined, y=model["w_f"].T), y=model["b_f"])
            )
            output_gate = mb.sigmoid(
                x=mb.add(x=mb.matmul(x=combined, y=model["w_o"].T), y=model["b_o"])
            )
            candidate = mb.tanh(
                x=mb.add(x=mb.matmul(x=combined, y=model["w_c"].T), y=model["b_c"])
            )
            cell = mb.add(x=mb.mul(x=forget_gate, y=cell), y=mb.mul(x=input_gate, y=candidate))
            hidden = mb.mul(x=output_gate, y=mb.tanh(x=cell))

        raw = mb.add(
            x=mb.matmul(x=hidden, y=model["w_out"].reshape(hidden_dim, 1)),
            y=model["output_bias"],
        )
        raw = mb.squeeze(x=raw, axes=[1])
        if model["target_mode"] == 1:
            score = mb.sigmoid(x=raw)
        else:
            score = mb.add(
                x=mb.mul(x=raw, y=np.float16(model["target_std"])),
                y=np.float16(model["target_mean"]),
            )
        return mb.cast(x=score, dtype="fp32", name="score")

    return program


def remove_path(path):
    if path.is_dir():
        shutil.rmtree(path)
    elif path.exists():
        path.unlink()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    parser.add_argument("--package", required=True)
    parser.add_argument("--compiled", required=True)
    parser.add_argument("--metadata", required=True)
    parser.add_argument("--batch-size", type=int, default=128)
    args = parser.parse_args()

    source_path = pathlib.Path(args.model).resolve()
    package_path = pathlib.Path(args.package).resolve()
    compiled_path = pathlib.Path(args.compiled).resolve()
    metadata_path = pathlib.Path(args.metadata).resolve()
    if args.batch_size < 1:
        raise ValueError("batch size must be positive")

    model = load_model(source_path)
    converted = ct.convert(
        build_program(model, args.batch_size),
        convert_to="mlprogram",
        compute_precision=ct.precision.FLOAT16,
        minimum_deployment_target=ct.target.macOS13,
    )
    converted.author = "mlai-trade"
    converted.short_description = "Validated fixed-batch LSTM inference for Apple Neural Engine"
    converted.input_description["sequence"] = "Normalized LSTM windows [batch, sequence, features]"
    converted.output_description["score"] = "Decoded return or direction score"

    package_path.parent.mkdir(parents=True, exist_ok=True)
    remove_path(package_path)
    remove_path(compiled_path)
    converted.save(str(package_path))
    subprocess.run(
        ["xcrun", "coremlcompiler", "compile", str(package_path), str(compiled_path.parent)],
        check=True,
    )
    generated = compiled_path.parent / f"{package_path.stem}.mlmodelc"
    if generated != compiled_path:
        remove_path(compiled_path)
        generated.rename(compiled_path)

    metadata = {
        "schema_version": 1,
        "source_model_sha256": hashlib.sha256(source_path.read_bytes()).hexdigest(),
        "batch_size": args.batch_size,
        "input_dim": model["input_dim"],
        "hidden_dim": model["hidden_dim"],
        "sequence_length": model["sequence_length"],
        "target_mode": "direction" if model["target_mode"] == 1 else "regression",
        "compute_units": "cpu_and_neural_engine",
        "compute_precision": "float16",
        "coremltools_version": ct.__version__,
        "validated": False,
        "neural_engine_operations": 0,
    }
    metadata_path.write_text(json.dumps(metadata, indent=2) + "\n")
    print(json.dumps(metadata))


if __name__ == "__main__":
    main()
