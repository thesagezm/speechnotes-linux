#!/usr/bin/env python3
"""A/B the PDF text layer against poppler on real books (read-only).

Runs our extractor (pdf-survey example) and pdftotext over the same page
ranges and scores both: common-word rate (how much reads as English),
per-page emptiness, and fused-word / missing-space symptoms.

Usage: python3 truepdf-ab-survey.py <pdf> <first> <last> [more ranges...]
The pdf-survey example must be built (cargo build --release -p sn-doc --examples).
"""
import re
import subprocess
import sys
import collections

WORDS = set()
for w in open('/usr/share/dict/american-english', encoding='utf-8', errors='ignore'):
    WORDS.add(w.strip().lower())

COMMON = {'the','of','and','to','in','is','that','it','for','as','with','was','on','are','by','this','be','from','or','an','which','one','had','not','but','what','all','were','when','we','there','can','been','more','if','will','would','about','their','other','new','some','could','time','these','two','may','then','do','first','any','my','now','such','like','our','over','man','me','even','most','after','also','made','has','its','where','who','patients','treatment','clinical','blood','cells','disease','symptoms','acute','chronic','risk','between','during','than','into','no','only','very','at','a'}


def score(text):
    tokens = re.findall(r"[A-Za-zÀ-ÿḀ-ỹ][A-Za-zÀ-ÿḀ-ỹ'’-]*", text)
    if not tokens:
        return dict(tokens=0, common=0.0, dict=0.0, fused=0, chars=len(text))
    lower = [t.lower().strip("'’-") for t in tokens]
    common = sum(1 for t in lower if t in COMMON)
    dictw = sum(1 for t in lower if t in WORDS)
    # fused words: a long token not in the dictionary that splits into two
    # dictionary words — "the lateral" fused as "thelateral"
    fused = 0
    for t in lower:
        if len(t) >= 10 and t not in WORDS and not t.isdigit():
            for k in range(3, len(t) - 2):
                if t[:k] in WORDS and t[k:] in WORDS:
                    fused += 1
                    break
    return dict(tokens=len(tokens), common=common, dict=dictw,
                fused=fused, chars=len(text))


def main():
    pdf = sys.argv[1]
    ranges = [(int(a), int(b)) for a, b in zip(sys.argv[2::2], sys.argv[3::2])]
    ours_total = collections.Counter()
    pop_total = collections.Counter()
    for first, last in ranges:
        pages = range(first, last + 1)
        ours = subprocess.run(
            [f"target/release/examples/pdf-survey", pdf, str(first), str(last)],
            capture_output=True, text=True).stdout
        pop = subprocess.run(
            ["pdftotext", "-f", str(first), "-l", str(last), pdf, "-"],
            capture_output=True, text=True).stdout
        # split into per-page
        ours_pages = re.split(r"===== page (\d+) =====", ours)
        ours_by = {int(ours_pages[i]): ours_pages[i + 1] for i in range(1, len(ours_pages) - 1, 2)}
        pop_by = {p: t for p, t in zip(pages, pop.split("\f"))}
        print(f"== pages {first}-{last}")
        print(f"{'page':>5} | {'ours tok':>8} {'dict':>6} {'fused':>5} | {'pop tok':>8} {'dict':>6} {'fused':>5}")
        empty_ours = 0
        for p in pages:
            so = score(ours_by.get(p, ""))
            sp = score(pop_by.get(p, ""))
            if so['tokens'] == 0:
                empty_ours += 1
            for k in so:
                ours_total[k] += so[k]
            for k in sp:
                pop_total[k] += sp[k]
            do_ = so['dict'] / so['tokens'] if so['tokens'] else 0.0
            dp = sp['dict'] / sp['tokens'] if sp['tokens'] else 0.0
            print(f"{p:>5} | {so['tokens']:>8} {do_:>6.3f} {so['fused']:>5} | {sp['tokens']:>8} {dp:>6.3f} {sp['fused']:>5}")
        if empty_ours:
            print(f"   ours EMPTY on {empty_ours}/{len(list(pages))} pages")
    print("== TOTALS")
    print("ours:", dict(ours_total))
    print("pop :", dict(pop_total))
    if ours_total['tokens'] and pop_total['tokens']:
        print(f"ours dict-rate {ours_total['dict']/ours_total['tokens']:.3f} vs pop {pop_total['dict']/pop_total['tokens']:.3f}")
        print(f"ours common-rate {ours_total['common']/ours_total['tokens']:.3f} vs pop {pop_total['common']/pop_total['tokens']:.3f}")
        print(f"ours fused {ours_total['fused']} vs pop {pop_total['fused']}")


if __name__ == '__main__':
    main()
