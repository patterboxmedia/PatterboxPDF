"""Render controlled CMYK-to-display previews with caller-supplied free tools."""
import argparse
from pathlib import Path
import sys

from pdf_proofing import ProofingError, render_preview


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('pdf', type=Path)
    parser.add_argument('--print-profile', type=Path, required=True)
    parser.add_argument('--display-profile', type=Path, required=True)
    parser.add_argument('--renderer', type=Path, required=True, help='CMS-enabled pdftoppm executable')
    parser.add_argument('--output', type=Path, required=True, help='new output directory')
    parser.add_argument('--dpi', type=float, default=144)
    parser.add_argument('--pages', help='comma-separated unique 1-based page numbers; default all')
    args = parser.parse_args(argv)
    try:
        pages = None if args.pages is None else [int(value) for value in args.pages.split(',')]
        render_preview(args.pdf, print_profile=args.print_profile,
            display_profile=args.display_profile, renderer=args.renderer,
            output=args.output, pages=pages, dpi=args.dpi)
    except (ProofingError, ValueError) as error:
        print(f'Preview failed: {error}', file=sys.stderr)
        return 1
    print(f'Preview complete: {args.output / "manifest.json"}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
