"""FastProto: readable, Pythonic protobuf messages backed by a Rust core.

Generated modules define plain ``@dataclass`` message types annotated with
``Scalar.*`` field types and decorated with :func:`message`, which wires the
class to the native (de)serialization engine.
"""

import sys
from collections.abc import Callable
from typing import Annotated, ClassVar, Self, cast, override

from ._core import Descriptor, compile_descriptor

__all__ = ["Message", "Scalar", "message"]


class _ScalarMeta:
    """Marker attached to a ``Scalar.*`` annotation recording the proto type.

    Purely informational: the actual wire type comes from the compiled
    descriptor, so this never affects encoding. It exists so tools and humans
    can recover the precise proto type from an annotation.
    """

    __slots__ = ("proto_name",)

    proto_name: str

    def __init__(self, proto_name: str) -> None:
        self.proto_name = proto_name

    @override
    def __repr__(self) -> str:
        return f"proto:{self.proto_name}"


class Scalar:
    """Namespace of proto scalar field types.

    Each alias is an ``Annotated`` wrapper over the underlying Python type, so
    ``Scalar.Int64`` type-checks exactly as ``int`` while still declaring, for
    the reader, that the field is a proto ``int64``.
    """

    Double = Annotated[float, _ScalarMeta("double")]
    Float = Annotated[float, _ScalarMeta("float")]
    Int32 = Annotated[int, _ScalarMeta("int32")]
    Int64 = Annotated[int, _ScalarMeta("int64")]
    UInt32 = Annotated[int, _ScalarMeta("uint32")]
    UInt64 = Annotated[int, _ScalarMeta("uint64")]
    SInt32 = Annotated[int, _ScalarMeta("sint32")]
    SInt64 = Annotated[int, _ScalarMeta("sint64")]
    Fixed32 = Annotated[int, _ScalarMeta("fixed32")]
    Fixed64 = Annotated[int, _ScalarMeta("fixed64")]
    SFixed32 = Annotated[int, _ScalarMeta("sfixed32")]
    SFixed64 = Annotated[int, _ScalarMeta("sfixed64")]
    Bool = Annotated[bool, _ScalarMeta("bool")]
    String = Annotated[str, _ScalarMeta("string")]
    Bytes = Annotated[bytes, _ScalarMeta("bytes")]


class Message:
    """Base class for generated message dataclasses.

    Provides the serialization API; the :func:`message` decorator attaches the
    compiled descriptor as ``__fastproto__``. Generated classes are plain
    ``@dataclass`` types that inherit from this, so ``to_bytes`` / ``from_bytes``
    are fully typed for callers.

    The ``_fastproto_unknown`` slot holds the raw wire bytes of fields the
    schema doesn't know about: the decoder stores them and the encoder re-emits
    them, so decode -> encode round-trips preserve fields added by newer
    producers (protobuf forward compatibility). It is deliberately *not* a
    dataclass field — adding an annotation here would make ``@dataclass`` treat
    it as an ``__init__`` parameter on every generated class.
    """

    __slots__ = ("_fastproto_unknown",)
    __fastproto__: ClassVar[Descriptor]

    def to_bytes(self) -> bytes:
        """Serialize this message to protobuf wire bytes."""
        _ensure_linked(type(self))
        return self.__fastproto__.encode(self)

    @classmethod
    def from_bytes(cls, data: bytes) -> Self:
        """Deserialize protobuf wire bytes into a new instance of ``cls``."""
        _ensure_linked(cls)
        return cls.__fastproto__.decode(cls, data)

    def which_oneof(self, name: str) -> str | None:
        """Return the set member of oneof group ``name``, or ``None`` if unset.

        Mirrors google-protobuf's ``WhichOneof``: members are plain ``T | None``
        fields, and this reports which one currently holds a value. Raises
        :class:`ValueError` if ``name`` is not a oneof group of this message.
        """
        for group, members in self.__fastproto__.oneofs():
            if group == name:
                return next((m for m in members if getattr(self, m) is not None), None)
        available = [group for group, _ in self.__fastproto__.oneofs()]
        msg = f"{type(self).__name__!r} has no oneof group {name!r}; got {available}"
        raise ValueError(msg)


def _resolve(namespace: dict[str, object], qualified: str) -> type:
    """Look up a (possibly nested) type by its qualified proto name.

    ``qualified`` is a full proto path minus the leading dot, e.g.
    ``pkg.Outer.Inner``. The package prefix is not reflected in the module
    namespace, and we can't tell from the name alone how many leading segments
    are package versus enclosing message. So we try each suffix in turn — take
    the first segment that names a module-level class, then walk the rest as
    attributes (into nested classes) — and use the first chain that resolves.
    """
    parts = qualified.split(".")
    for start in range(len(parts)):
        obj = namespace.get(parts[start])
        if obj is None:
            continue
        try:
            for attr in parts[start + 1 :]:
                obj = getattr(obj, attr)
        except AttributeError:
            continue
        return cast("type", obj)
    msg = f"cannot resolve referenced type {qualified!r}"
    raise LookupError(msg)


def _ensure_linked(cls: type[Message]) -> None:
    """Resolve enum/message references for ``cls`` and the classes it references.

    Deferred until first use because sibling classes may not exist when the
    decorator runs. ``link`` marks the descriptor linked *before* we recurse,
    which breaks reference cycles (A -> B -> A).
    """
    descriptor = cls.__fastproto__
    if descriptor.is_linked:
        return

    references = descriptor.ref_fields()
    if not references:
        descriptor.link({})  # no enum/message fields — nothing to resolve
        return

    namespace = vars(sys.modules[cls.__module__])
    resolved = {number: _resolve(namespace, name) for number, name in references}
    descriptor.link(resolved)

    for target in resolved.values():
        if hasattr(target, "__fastproto__"):
            _ensure_linked(cast("type[Message]", target))


def message[MessageT: Message](
    descriptor: bytes,
) -> Callable[[type[MessageT]], type[MessageT]]:
    """Bind a :class:`Message` dataclass to its compiled descriptor.

    Parses the embedded ``DescriptorProto`` bytes once and stores the result on
    the class as ``__fastproto__`` for :meth:`Message.to_bytes` /
    :meth:`Message.from_bytes` to use.
    """
    compiled = compile_descriptor(descriptor)

    def bind(cls: type[MessageT]) -> type[MessageT]:
        cls.__fastproto__ = compiled
        return cls

    return bind
