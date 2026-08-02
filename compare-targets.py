#!/usr/bin/env python3
"""Compare what each target's fdk-aac-e2e produced, and decide whether they agree.

Replaces an earlier `bit-exact` job that required all four targets to encode byte-identical
bitstreams. That assertion was false and CI proved it on the first run: fdk-aac is usually
described as fixed-point integer code throughout, but on x86 `fixmul.h` and
`fixpoint_math.h` pull in `x86/fixmul_x86.h` and `x86/fixpoint_math_x86.h`, replacing
`sqrtFixp`, `invSqrtNorm2`, both overloads of `invFixp` and `schur_div` with x86-specific
implementations. aarch64 uses the generic C ones. Different algorithms round differently, the
encoder makes slightly different quantisation decisions, and the bitstream differs — by
design, upstream, not a miscompile.

Two things it is emphatically not, both worth knowing before anyone proposes turning an
optimization off to make the numbers agree. It is not floating point: `-ffp-contract=off` on
the x86-64-v3 build changes not one digest. And it is not the CPU floors, which the full
matrix settles — `windows-x86_64-msvc` built by MSVC at `/arch:AVX2` is byte-identical to
`linux-x86_64` built by GCC at `-march=x86-64-v3`, and `macos-arm64` at `-mcpu=apple-m1` is
byte-identical to `linux-aarch64` at no floor at all, while the same GCC across the two
architectures differs. The boundary is the architecture, so lowering a floor would cost speed
and change nothing.

So the targets are compared at the strength they actually agree at, which is three different
strengths and is why this is a script rather than a `diff`:

  structure   exact, across every target including across architectures. Granule, decoded
              sample rate, decoded channel count, access-unit count and decoded length follow
              from the format and the input, not from arithmetic.

  size        the encoded bitstream length, within a tolerance. Observed spread between
              x86_64 and aarch64 is under 0.03%; the tolerance is far looser than that,
              because its job is to catch a target that encoded something structurally
              different, not to police the last byte.

  audio       decoded audio, as an SNR against a reference target. This is the claim that
              replaces byte-identity: whatever the bitstreams do, the *sound* the four
              archives produce must be the same to within a threshold.

  digests     byte-identical, but only between targets of the same architecture. That holds
              strongly — the same seven digests come from a Docker container on an
              Apple-silicon Mac and from a GitHub `ubuntu-24.04-arm` runner, built by two
              different GCC versions into two measurably different libraries.

Standard library only, so a CI runner needs nothing installed.
"""

import argparse
import math
import struct
import sys
import wave
from pathlib import Path

# Which architecture each target is, for the same-architecture digest comparison. Named
# explicitly rather than inferred from the target string: a new target should have to state
# its architecture here, not have one guessed from its name.
ARCHITECTURE = {
    "macos-arm64": "arm64",
    "linux-aarch64": "arm64",
    "linux-x86_64": "x86_64",
    "windows-x86_64-msvc": "x86_64",
}

# The exactly-equal fields of a structure.txt line. `bytes` is deliberately not among them.
EXACT_FIELDS = ("granule", "rate", "channels", "access_units", "decoded_samples")

# Per-configuration SNR floors in dB, and the measurement each was set from.
#
# Per-configuration rather than one global number, for the same reason `common::Signal` in the
# test suite carries a `corr_floor` per signal class: one threshold would either pass a broken
# AAC-LC path or fail a working HE-AAC one. The measured spread between linux-x86_64 and
# linux-aarch64 is 29 dB to 79 dB depending entirely on whether SBR is running.
#
# HE-AAC is the low one and is not a defect. SBR does not code the high band as a waveform, it
# synthesises it from parameters, so a marginally different parameter decision rebuilds that
# octave differently while preserving its energy — which is what a waveform SNR is least
# equipped to score. Measured at 29.4 dB, the same pair correlates at 0.999423 with an RMS
# ratio of 1.00067: the same audio, not the same samples.
#
# Every floor sits well under what was measured, because a threshold tuned to today's number
# is a check that fails on the next compiler.
SNR_FLOOR = {
    "lc-16k-mono-32k-adts": 35.0,  # measured 49.1
    "lc-44k1-stereo-128k-adts": 38.0,  # measured 53.3
    "lc-48k-stereo-vbr3-adts": 55.0,  # measured 79.1
    "lc-48k-mono-64k-raw": 35.0,  # measured 48.5
    "he-32k-stereo-48k-adts": 18.0,  # measured 29.4 — SBR, see above
    "hev2-44k1-stereo-32k-loas": 38.0,  # measured 53.1
    "eld-32k-mono-64k-raw": 48.0,  # measured 66.3
}

# For a configuration not in the table — a new one added to the e2e battery without a floor
# being measured for it. Deliberately low: an unmeasured configuration should not fail the
# build on a number nobody chose, and the correlation floor still applies to it.
DEFAULT_SNR_FLOOR = 18.0

# Applied to every configuration, alongside the SNR floor rather than instead of it, because
# the two fail on different things. SNR moves with level *and* shape; correlation ignores
# level entirely. A decode at half the right amplitude is a 6 dB SNR and a correlation of 1.0;
# a decode with the right energy in the wrong places is the reverse. Requiring both is what
# distinguishes "the same audio" from "audio with the same statistics".
#
# 0.995, which is about 20 dB for two signals at the same level — so where the SNR floor has
# to be loose because SBR is running, this is the constraint that still bites. The lowest
# correlation measured across the architecture boundary is 0.999423, on that same HE-AAC
# configuration.
CORRELATION_FLOOR = 0.995


def fail(message):
    print(f"::error::{message}")
    return False


def read_structure(path):
    """Parse structure.txt into {label: {field: int}}, preserving order."""
    entries = {}
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        label, *rest = line.split()
        entries[label] = {k: int(v) for k, v in (f.split("=", 1) for f in rest)}
    return entries


def read_wav(path):
    """Return (rate, channels, samples) with samples as a list of ints."""
    with wave.open(str(path), "rb") as w:
        if w.getsampwidth() != 2:
            raise ValueError(f"{path}: expected 16-bit, got {w.getsampwidth() * 8}-bit")
        frames = w.readframes(w.getnframes())
        count = len(frames) // 2
        return w.getframerate(), w.getnchannels(), struct.unpack(f"<{count}h", frames)


def snr_db(reference, other):
    """Signal-to-noise ratio of `other` against `reference`, in dB.

    `inf` when the two are identical, which is the normal result for two targets of the same
    architecture and is reported rather than special-cased — a table where some rows read
    `identical` and others read 53.3 dB says exactly where the architecture boundary is.
    """
    signal = sum(float(s) * s for s in reference)
    noise = sum((float(a) - b) ** 2 for a, b in zip(reference, other))
    if noise == 0:
        return math.inf
    if signal == 0:
        return -math.inf
    return 10.0 * math.log10(signal / noise)


def correlation(a, b):
    """Pearson correlation, the gain-insensitive half of the comparison."""
    n = len(a)
    mean_a = sum(a) / n
    mean_b = sum(b) / n
    num = sum((x - mean_a) * (y - mean_b) for x, y in zip(a, b))
    dev_a = math.sqrt(sum((x - mean_a) ** 2 for x in a))
    dev_b = math.sqrt(sum((y - mean_b) ** 2 for y in b))
    if dev_a == 0 or dev_b == 0:
        return 0.0
    return num / (dev_a * dev_b)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path, help="directory holding one subdirectory per target")
    parser.add_argument("--min-snr", type=float, default=None,
                        help="override every per-configuration SNR floor with this one, in dB")
    parser.add_argument("--size-tolerance", type=float, default=0.02,
                        help="fractional bitstream-size spread allowed between targets")
    parser.add_argument("--expected", type=int, required=True,
                        help="how many targets must be present")
    args = parser.parse_args()

    targets = sorted(d.name for d in args.root.iterdir() if d.is_dir())
    print(f"collected {len(targets)} target(s): {', '.join(targets) or '(none)'}\n")

    # Before anything is compared: a partial matrix must not be allowed to "agree". With
    # fail-fast off, a target that died before reporting would otherwise leave the survivors
    # to pass by comparing themselves with each other.
    if len(targets) != args.expected:
        return fail(
            f"expected {args.expected} target(s) but found {len(targets)} — a target failed "
            f"before it could report, so there is nothing meaningful to compare"
        )

    ok = True
    reference = targets[0]
    structures = {}
    for target in targets:
        path = args.root / target / "structure.txt"
        if not path.is_file():
            ok = fail(f"{target}: no structure.txt")
            continue
        structures[target] = read_structure(path)

    if not ok:
        return False

    # ---------------------------------------------------------------- structure, exactly
    print("structure — must be identical on every target")
    reference_structure = structures[reference]
    for label, fields in reference_structure.items():
        summary = " ".join(f"{k}={fields[k]}" for k in EXACT_FIELDS)
        print(f"  {label:<26} {summary}")
    for target in targets[1:]:
        if set(structures[target]) != set(reference_structure):
            missing = set(reference_structure) - set(structures[target])
            extra = set(structures[target]) - set(reference_structure)
            ok = fail(
                f"{target}: configurations differ from {reference} "
                f"(missing {sorted(missing)}, unexpected {sorted(extra)})"
            )
            continue
        for label, fields in reference_structure.items():
            for field in EXACT_FIELDS:
                mine, theirs = fields[field], structures[target][label][field]
                if mine != theirs:
                    ok = fail(
                        f"{target}: {label}.{field} is {theirs}, but {reference} says {mine} — "
                        f"this field follows from the format and cannot legitimately differ"
                    )
    print(f"  -> {'identical on all targets' if ok else 'MISMATCH'}\n")

    # ---------------------------------------------------------------- size, within tolerance
    print(f"bitstream size — spread must stay under {args.size_tolerance:.1%}")
    for label in reference_structure:
        sizes = {t: structures[t][label]["bytes"] for t in targets}
        low, high = min(sizes.values()), max(sizes.values())
        spread = (high - low) / low if low else 0.0
        marker = "ok " if spread <= args.size_tolerance else "BAD"
        print(f"  {marker} {label:<26} {low}..{high} ({spread:.4%})")
        if spread > args.size_tolerance:
            worst = max(sizes, key=lambda t: abs(sizes[t] - sizes[reference]))
            ok = fail(
                f"{label}: bitstream sizes span {spread:.2%} across targets "
                f"({worst} encoded {sizes[worst]} bytes against {reference}'s {sizes[reference]})"
            )
    print()

    # ---------------------------------------------------------------- digests, per architecture
    print("bitstream digests — byte-identical within an architecture")
    by_architecture = {}
    for target in targets:
        by_architecture.setdefault(ARCHITECTURE.get(target, target), []).append(target)
    for architecture, members in sorted(by_architecture.items()):
        if len(members) < 2:
            print(f"  {architecture}: only {members[0]} — nothing to compare against")
            continue
        head = members[0]
        expected = (args.root / head / "digests.txt").read_text()
        for other in members[1:]:
            actual = (args.root / other / "digests.txt").read_text()
            if actual == expected:
                print(f"  ok  {architecture}: {other} matches {head}")
            else:
                ok = fail(
                    f"{other} and {head} are both {architecture} and must encode identical "
                    f"bytes, but their digests differ"
                )
    print()

    # ---------------------------------------------------------------- audio, as an SNR
    print(f"decoded audio — against {reference}, correlation floor {CORRELATION_FLOOR}")
    for label in reference_structure:
        name = f"{label}-decoded.wav"
        floor = args.min_snr if args.min_snr is not None else SNR_FLOOR.get(
            label, DEFAULT_SNR_FLOOR
        )
        try:
            rate, channels, expected_pcm = read_wav(args.root / reference / "audio" / name)
        except (OSError, ValueError, wave.Error) as e:
            ok = fail(f"{reference}: cannot read {name}: {e}")
            continue
        for target in targets[1:]:
            try:
                other_rate, other_channels, actual = read_wav(args.root / target / "audio" / name)
            except (OSError, ValueError, wave.Error) as e:
                ok = fail(f"{target}: cannot read {name}: {e}")
                continue
            if (other_rate, other_channels) != (rate, channels):
                ok = fail(
                    f"{target}: {label} decodes to {other_rate} Hz/{other_channels}ch but "
                    f"{reference} gives {rate} Hz/{channels}ch"
                )
                continue
            if len(actual) != len(expected_pcm):
                ok = fail(
                    f"{target}: {label} decoded to {len(actual)} samples against "
                    f"{reference}'s {len(expected_pcm)}"
                )
                continue
            value = snr_db(expected_pcm, actual)
            corr = correlation(expected_pcm, actual)
            good = value >= floor and corr >= CORRELATION_FLOOR
            shown = "  identical" if value == math.inf else f"{value:6.1f} dB"
            print(
                f"  {'ok ' if good else 'BAD'} {label:<26} {target:<20} "
                f"{shown} (floor {floor:g})  corr {corr:.6f}"
            )
            if value < floor:
                ok = fail(
                    f"{target}: {label} decodes {value:.1f} dB from {reference}'s output, "
                    f"below the {floor:g} dB floor — these two archives do not agree on what "
                    f"this configuration sounds like"
                )
            if corr < CORRELATION_FLOOR:
                ok = fail(
                    f"{target}: {label} correlates {corr:.6f} with {reference}'s output, "
                    f"below the {CORRELATION_FLOOR} floor — the decoded shape differs, which "
                    f"no amount of legitimate rounding accounts for"
                )
    print()

    if ok:
        print(f"all {len(targets)} targets agree")
    else:
        print("::error::targets disagree — see above")
    return ok


if __name__ == "__main__":
    sys.exit(0 if main() else 1)
