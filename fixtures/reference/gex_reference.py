"""Independent reference implementation of gex-core, for golden tests.

Reads every ``<name>.chain.json`` in a directory and writes
``<name>.expected.json`` beside it. Standard library only.

Usage: py -3 -I fixtures/reference/gex_reference.py fixtures/golden

Conventions (the definitions, shared with docs/CONVENTIONS.md): dealer signs
calls +1 / puts -1, GEX = gamma*OI*mult*S^2*0.01, DEX = delta*OI*mult*S,
vanna exposure = vanna*OI*mult*S*0.01, charm exposure = charm*OI*mult*S/365
with charm = -dDelta/dT, a one-minute floor on T for exposures, contracts
with iv <= 0 skipped. A zero-gamma crossing is the midpoint of an interval
in [0.9 S, 1.1 S] where |GEX| <= negligible_gex that separates positive from
negative GEX.

Methods (deliberately different from the Rust code, so agreement means
something): greeks use the textbook Black-Scholes-Merton formulas and
math.erfc. The flip is found by sweeping a 4001-point grid, not 201, and
locating roots and interval edges with Illinois regula falsi on GEX and on
|GEX| - threshold, not bisection. Sums are also reported as sums of absolute
contributions (``*_abs``) so the Rust side can scale its tolerance when
positive and negative terms cancel.
"""

import json
import math
import sys
from pathlib import Path

DAYS_PER_YEAR = 365.0
MIN_TIME = 1.0 / (DAYS_PER_YEAR * 1440.0)
CALL_SIGN, PUT_SIGN = 1.0, -1.0
GRID_LOWER, GRID_UPPER = 0.9, 1.1
NEGLIGIBLE_GEX_FRACTION = 1e-9
DENSE_POINTS = 4001
ROOT_TOL = 1e-12
ROOT_MAX_STEPS = 500
EXPIRY_TOLERANCE = 1e-9
TIE_TOLERANCE = 1e-12


def norm_cdf(x):
    return 0.5 * math.erfc(-x / math.sqrt(2.0))


def norm_pdf(x):
    return math.exp(-0.5 * x * x) / math.sqrt(2.0 * math.pi)


def greeks(kind, s, k, t, r, q, sigma):
    """Price and greeks; limits when sigma <= 0 or t <= 0."""
    zero = dict(price=0.0, delta=0.0, gamma=0.0, vega=0.0, vanna=0.0, charm=0.0)
    if sigma > 0.0 and t > 0.0:
        vol = sigma * math.sqrt(t)
        d1 = (math.log(s / k) + (r - q + 0.5 * sigma * sigma) * t) / vol
        if vol > 0.0 and math.isfinite(d1):
            d2 = d1 - vol
            dq, dr = math.exp(-q * t), math.exp(-r * t)
            pdf = norm_pdf(d1)
            shared = dq * pdf * (2.0 * (r - q) * t - d2 * vol) / (2.0 * t * vol)
            g = dict(
                gamma=dq * pdf / (s * vol),
                vega=s * dq * pdf * math.sqrt(t),
                vanna=-dq * pdf * d2 / sigma,
            )
            if kind == "call":
                g.update(
                    price=s * dq * norm_cdf(d1) - k * dr * norm_cdf(d2),
                    delta=dq * norm_cdf(d1),
                    charm=q * dq * norm_cdf(d1) - shared,
                )
            else:
                g.update(
                    price=k * dr * norm_cdf(-d2) - s * dq * norm_cdf(-d1),
                    delta=-dq * norm_cdf(-d1),
                    charm=-q * dq * norm_cdf(-d1) - shared,
                )
            return g
    tt = max(t, 0.0)
    dq = math.exp(-q * tt)
    fs, fk = s * dq, k * math.exp(-r * tt)
    step = 1.0 if fs > fk else 0.5 if fs == fk else 0.0
    if kind == "call":
        zero.update(price=max(fs - fk, 0.0), delta=dq * step)
    else:
        zero.update(price=max(fk - fs, 0.0), delta=-dq * (1.0 - step))
    return zero


def sign(kind):
    return CALL_SIGN if kind == "call" else PUT_SIGN


def exposure_greeks(chain, c, spot):
    t = max(c["expiry"], MIN_TIME)
    return greeks(c["kind"], spot, c["strike"], t, chain["rate"], chain["div_yield"], c["iv"])


def usable(chain):
    return [c for c in chain["contracts"] if c["iv"] > 0.0]


def gex_function(chain):
    """Total GEX as a function of spot, with per-contract constants hoisted
    so the dense sweep stays fast."""
    r, q, mult = chain["rate"], chain["div_yield"], chain["multiplier"]
    terms = []
    for c in usable(chain):
        t = max(c["expiry"], MIN_TIME)
        sigma = c["iv"]
        vol = sigma * math.sqrt(t)
        w = sign(c["kind"]) * c["open_interest"] * mult
        terms.append((w * math.exp(-q * t), c["strike"], vol, (r - q + 0.5 * sigma * sigma) * t))

    def gex(s):
        total = 0.0
        for w_dq, k, vol, drift in terms:
            d1 = (math.log(s / k) + drift) / vol
            total += w_dq * norm_pdf(d1) / (s * vol) * s * s * 0.01
        return total

    return gex


def per_strike(chain):
    spot, mult = chain["spot"], chain["multiplier"]
    rows = {}
    keys = ("call_gex", "put_gex", "gex", "dex", "vanna", "charm")
    for c in chain["contracts"]:
        row = rows.setdefault(
            c["strike"],
            dict(strike=c["strike"], call_oi=0, put_oi=0,
                 **{k: 0.0 for k in keys}, **{k + "_abs": 0.0 for k in keys}),
        )
        row["call_oi" if c["kind"] == "call" else "put_oi"] += c["open_interest"]
        if c["iv"] <= 0.0:
            continue
        g = exposure_greeks(chain, c, spot)
        w = sign(c["kind"]) * c["open_interest"] * mult
        parts = dict(
            gex=w * g["gamma"] * spot * spot * 0.01,
            dex=w * g["delta"] * spot,
            vanna=w * g["vanna"] * spot * 0.01,
            charm=w * g["charm"] * spot / DAYS_PER_YEAR,
        )
        parts["call_gex" if c["kind"] == "call" else "put_gex"] = parts["gex"]
        for k, v in parts.items():
            row[k] += v
            row[k + "_abs"] += abs(v)
    return [rows[k] for k in sorted(rows)]


def negligible_gex(chain):
    """NEGLIGIBLE_GEX_FRACTION times the sum of each usable contract's peak
    dollar gamma (spot at its own strike)."""
    mult = chain["multiplier"]
    peak = 0.0
    for c in usable(chain):
        k = c["strike"]
        g = exposure_greeks(chain, c, k)
        peak += abs(sign(c["kind"]) * c["open_interest"] * mult) * g["gamma"] * k * k * 0.01
    return NEGLIGIBLE_GEX_FRACTION * peak


def illinois(g, a, b, tol):
    """Root of g in [a, b], where g(a) and g(b) have opposite signs, by the
    Illinois variant of regula falsi."""
    ga, gb = g(a), g(b)
    if ga == 0.0:
        return a
    if gb == 0.0:
        return b
    for _ in range(ROOT_MAX_STEPS):
        c = b - gb * (b - a) / (gb - ga)
        gc = g(c)
        if gc == 0.0:
            return c
        if (gc < 0.0) != (gb < 0.0):
            a, ga = b, gb
        else:
            ga *= 0.5
        b, gb = c, gc
        if abs(b - a) <= tol:
            break
    return b


def zero_gamma(chain):
    """Crossings on a 4001-point sweep. For each sign change between
    consecutive non-negligible samples, find a point z with |GEX| <=
    threshold between them (a negligible sample, or else the root of GEX),
    then the interval's edges as roots of |GEX| - threshold on each side of
    z. The crossing is the edges' midpoint."""
    spot = chain["spot"]
    threshold = negligible_gex(chain)
    if threshold <= 0.0:
        return None
    f = gex_function(chain)
    excess = lambda s: abs(f(s)) - threshold
    tol = ROOT_TOL * spot
    last = DENSE_POINTS - 1
    xs = [spot * (GRID_LOWER + (GRID_UPPER - GRID_LOWER) * j / last) for j in range(DENSE_POINTS)]
    values = [f(x) for x in xs]
    significant = [j for j, v in enumerate(values) if abs(v) > threshold]
    crossings = []
    for i, k in zip(significant, significant[1:]):
        if (values[i] < 0.0) == (values[k] < 0.0):
            continue
        z = xs[i + 1] if k > i + 1 else illinois(f, xs[i], xs[k], tol)
        left = z if excess(z) == 0.0 else illinois(excess, xs[i], z, tol)
        right = z if excess(z) == 0.0 else illinois(excess, z, xs[k], tol)
        crossings.append(0.5 * (left + right))
    if not crossings:
        return None
    level = min(crossings, key=lambda x: (abs(x - spot), x))
    return dict(level=level, crossings=crossings)


def nearest(strikes, spot):
    return min(strikes, key=lambda k: (abs(k - spot), k))


def walls(rows, spot):
    def pick(key):
        live = [r for r in rows if r[key] != 0.0]
        if not live:
            return None
        best = max(abs(r[key]) for r in live)
        return nearest([r["strike"] for r in live if abs(r[key]) == best], spot)

    return dict(call_wall=pick("call_gex"), put_wall=pick("put_gex"))


def expiries(chain):
    out = []
    for e in sorted(c["expiry"] for c in chain["contracts"]):
        if not out or e - out[-1] >= EXPIRY_TOLERANCE:
            out.append(e)
    return out


def at_expiry(chain, e):
    """Contracts in the expiry group e falls in: the last representative r <= e
    with e - r < tolerance, else the first above e within tolerance."""
    reps = expiries(chain)
    below = [i for i, r in enumerate(reps) if r <= e and e - r < EXPIRY_TOLERANCE]
    above = [i for i, r in enumerate(reps) if r > e and r - e < EXPIRY_TOLERANCE]
    if below:
        i = below[-1]
    elif above:
        i = above[0]
    else:
        return []
    end = reps[i + 1] if i + 1 < len(reps) else math.inf
    return [c for c in chain["contracts"] if reps[i] <= c["expiry"] < end]


def max_pain(chain, e):
    cs = at_expiry(chain, e)
    strikes = sorted({c["strike"] for c in cs})

    def pain(p):
        total = 0.0
        for c in cs:
            intrinsic = max(p - c["strike"], 0.0) if c["kind"] == "call" else max(c["strike"] - p, 0.0)
            total += intrinsic * c["open_interest"]
        return total

    values = {k: pain(k) for k in strikes}
    low = min(values.values())
    return nearest([k for k, v in values.items() if v - low <= TIE_TOLERANCE * abs(low)], chain["spot"])


def mid(c):
    bid, ask = c.get("bid"), c.get("ask")
    if bid is not None and ask is not None and ask > 0.0:
        return 0.5 * (bid + ask)
    return None


def expected_move(chain, e):
    cs = at_expiry(chain, e)
    calls = {c["strike"]: c for c in sorted(cs, key=canonical, reverse=True) if c["kind"] == "call"}
    puts = {c["strike"]: c for c in sorted(cs, key=canonical, reverse=True) if c["kind"] == "put"}
    both = sorted(set(calls) & set(puts))
    if not both:
        return dict(error="NoStraddle")
    spot = chain["spot"]
    k = nearest(both, spot)

    def leg(c):
        m = mid(c)
        if m is not None:
            return m, "Quote"
        g = greeks(c["kind"], spot, c["strike"], c["expiry"], chain["rate"], chain["div_yield"], c["iv"])
        return g["price"], "Model"

    call_price, call_source = leg(calls[k])
    put_price, put_source = leg(puts[k])
    straddle = call_price + put_price
    return dict(expiry=calls[k]["expiry"], strike=k, straddle=straddle,
                lower=spot - straddle, upper=spot + straddle,
                call_source=call_source, put_source=put_source)


def canonical(c):
    """Same key as the Rust canonical order (a missing quote sorts first);
    reversed sorts keep the first."""

    def quote(x):
        return (0, 0.0) if x is None else (1, x)

    return (c["strike"], c["kind"] == "put", c["expiry"], c["iv"], c["open_interest"],
            c.get("volume", 0), quote(c["bid"]), quote(c["ask"]))


def analyze(chain):
    chain.setdefault("multiplier", 100.0)
    for c in chain["contracts"]:
        c.setdefault("bid", None)
        c.setdefault("ask", None)
    spot = chain["spot"]
    rows = per_strike(chain)
    totals = {}
    for k in ("gex", "dex", "vanna", "charm"):
        totals["total_" + k] = sum(r[k] for r in rows)
        totals["total_" + k + "_abs"] = sum(r[k + "_abs"] for r in rows)
    regime = "PositiveGamma" if totals["total_gex"] > 0 else "NegativeGamma" if totals["total_gex"] < 0 else "Neutral"
    contract_greeks = [
        greeks(c["kind"], spot, c["strike"], c["expiry"], chain["rate"], chain["div_yield"], c["iv"])
        for c in chain["contracts"]
    ]
    return dict(
        config=dict(min_time=MIN_TIME, call_sign=CALL_SIGN, put_sign=PUT_SIGN,
                    flip_range=[GRID_LOWER, GRID_UPPER], flip_dense_points=DENSE_POINTS,
                    flip_method="illinois", negligible_fraction=NEGLIGIBLE_GEX_FRACTION),
        contracts=contract_greeks,
        per_strike=rows,
        **totals,
        regime=regime,
        skipped=len(chain["contracts"]) - len(usable(chain)),
        zero_gamma=zero_gamma(chain) if usable(chain) else None,
        walls=walls(rows, spot),
        expiries=[dict(expiry=e, max_pain=max_pain(chain, e), expected_move=expected_move(chain, e))
                  for e in expiries(chain)],
    )


def main(argv):
    if len(argv) != 2:
        print(__doc__.strip().splitlines()[3], file=sys.stderr)
        return 2
    directory = Path(argv[1])
    paths = sorted(directory.glob("*.chain.json"))
    if not paths:
        print(f"no *.chain.json files in {directory}", file=sys.stderr)
        return 1
    for path in paths:
        chain = json.loads(path.read_text(encoding="utf-8"))
        out = path.with_name(path.name.replace(".chain.json", ".expected.json"))
        text = json.dumps(analyze(chain), indent=1, allow_nan=False)
        out.write_text(text + "\n", encoding="utf-8", newline="\n")
        print(out)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
