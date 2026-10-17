# Generated-output rules

PatterboxPDF implements a bounded PDF 1.6 generation interface and a selected-CMYK
PDF/X-4 output mode. Public checks describe the structures this interface emits.
They do not certify arbitrary PDFs or establish unrestricted PDF/X-4 conformance.

The current output-rule catalog binds each checked rule to implementation paths,
an independent evaluator, exact positive PDFs and exercised negative controls.
Four current evidence records cover supported output, CMYK preview behavior,
Linux execution and Windows execution. The catalog audit verifies identities and
verdict consistency; it does not rerun a PDF analysis.

The current catalog passes with 37 rules, four exact positive PDFs, 37 exercised negative controls and three source-font metric checks. Native Windows and Ubuntu receipts also pass, including fresh extracted-source execution.

pdfx4-inventory.json retains earlier topic and writer-contract bindings.
The supported-output catalog is a separate finite verification contract.
Historical receipts describe their original source and exact PDFs. Changed output
requires new analysis.

Run python scripts/verify-output-rules.py for current catalog bindings and
python scripts/verify-requirements.py for retained topic bindings. The public
verify-pdfx-supported-output.py inspector reads exact PDF bytes with independent
PDF/font/profile tools and qpdf. Its controls exercise rejected and valid output.
Native worker tests separately establish preparation and process confinement.

See [supported output](supported-output-review.md),
[technical references](public-technical-sources.md),
[color behavior](../color-behavior.md) and
[platform support](../supported-platforms.md) for the contracts and practical limits.
