"""Cross-file (`import`) references resolve through generated sibling modules.

Regression for the generator ignoring `file.dependency`: a message/enum defined
in an imported proto used to be annotated by short name with no import emitted,
so the type was unresolvable at runtime. Covers a singular message, a repeated
message, a map value, and an enum -- all defined in `imports_dep.proto`.
"""

from tests.generated.imports_dep_pb import Currency, Money
from tests.generated.imports_main_pb import Invoice


def test_cross_file_roundtrip() -> None:
    invoice = Invoice(
        total=Money(amount=100, currency=Currency.CURRENCY_USD),
        items=[Money(amount=1), Money(amount=2)],
        ledger={"jan": Money(amount=5, currency=Currency.CURRENCY_USD)},
        preferred=Currency.CURRENCY_USD,
    )
    back = Invoice.from_bytes(invoice.to_bytes())
    assert back == invoice
    assert isinstance(back.total, Money)
    assert isinstance(back.items[0], Money)
    assert isinstance(back.ledger["jan"], Money)
    assert isinstance(back.preferred, Currency)
