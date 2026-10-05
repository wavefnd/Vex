#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Check tracked Markdown file links and local heading anchors without a network."""
import re
import argparse
import concurrent.futures
import urllib.error
import urllib.request
import subprocess
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]


def anchors(text):
    result, seen = set(), {}
    for heading in re.findall(r'^#{1,6}\s+(.+?)\s*#*$', text, re.M):
        heading = re.sub(r'[`*_]', '', heading).lower()
        slug = re.sub(r'[^\w\- ]', '', heading).replace(' ', '-')
        count = seen.get(slug, 0)
        seen[slug] = count + 1
        result.add(slug + (f'-{count}' if count else ''))
    result.update(re.findall(r'(?:id|name)=["\']([^"\']+)', text))
    return result


def check(paths):
    errors = []
    for path in paths:
        text = re.sub(r'^```.*?^```\s*$', '', path.read_text(encoding='utf-8'), flags=re.S | re.M)
        for url in re.findall(r'\[[^\]]*\]\(([^\s)]+)(?:\s+"[^"]*")?\)', text):
            parsed = urlsplit(url.strip('<>'))
            if parsed.scheme or parsed.netloc:
                continue
            target = (path.parent / unquote(parsed.path)).resolve() if parsed.path else path
            if not target.exists():
                errors.append(f'{path.relative_to(ROOT)}: missing {url}')
            elif parsed.fragment and target.suffix.lower() == '.md' and unquote(parsed.fragment) not in anchors(target.read_text(encoding='utf-8')):
                errors.append(f'{path.relative_to(ROOT)}: missing heading {url}')
    return errors


def external(paths):
    urls = set()
    for path in paths:
        text = re.sub(r'^```.*?^```\s*$', '', path.read_text(encoding='utf-8'), flags=re.S | re.M)
        for url in re.findall(r'\[[^\]]*\]\((https?://[^\s)]+)\)', text):
            parsed = urlsplit(url)
            if parsed.hostname in {'example.com', 'example.invalid'} or (parsed.hostname or '').endswith('.invalid'):
                continue  # Deliberately non-resolving examples, never project links.
            urls.add(url.split('#', 1)[0])
    def probe(url):
        try:
            request = urllib.request.Request(url, headers={'User-Agent': 'Vex-documentation-check'}, method='HEAD')
            with urllib.request.urlopen(request, timeout=20):
                pass
        except (OSError, ValueError) as error:
            return f'{url}: {error}'
        return None
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        return [error for error in pool.map(probe, sorted(urls)) if error]


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--external', action='store_true')
    args = parser.parse_args()
    names = subprocess.check_output(['git', 'ls-files', '-co', '--exclude-standard', '-z'], cwd=ROOT).decode().split('\0')
    paths = [ROOT / p for p in sorted(set(names)) if p.endswith('.md') and (ROOT / p).is_file()]
    errors = check(paths)
    if args.external:
        errors.extend(external(paths))
    if errors:
        raise SystemExit('\n'.join(errors))
    print('Markdown file links and local anchors passed.' + (' External URLs passed.' if args.external else ''))
