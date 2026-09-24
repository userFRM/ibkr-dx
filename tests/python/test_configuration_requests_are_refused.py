"""Configuration requests answer through error under the stated request id."""

import pytest

from ibkr_dx import EClient, EWrapper


MESSAGE = ("Configuration access via API is not available. Please refer to the "
           "application interface to view or update your settings.")


class Request:
    def __init__(self, req_id=None):
        self.reqId = req_id

    def HasField(self, field):
        assert field == "reqId"
        return self.reqId is not None


class Errors(EWrapper):
    def __init__(self):
        super().__init__()
        self.seen = []

    def error(self, req_id, error_time, code, message, advanced_order_reject_json=""):
        self.seen.append((req_id, code, message))


@pytest.mark.parametrize("name,keyword", [
    ("reqConfigProtoBuf", "configRequestProto"),
    ("updateConfigProtoBuf", "updateConfigRequestProto"),
    ("req_config_proto_buf", "config_request_proto"),
    ("update_config_proto_buf", "update_config_request_proto"),
])
@pytest.mark.parametrize("req_id", [219, -1, None])
def test_configuration_access_is_refused_under_its_request_id(name, keyword, req_id):
    wrapper = Errors()
    client = EClient(wrapper)
    client._test_connect("DU1")
    getattr(client, name)(**{keyword: Request(req_id)})
    unstated = 2**31 - 1 if name.startswith("update") else 0
    assert wrapper.seen == [(unstated if req_id is None else req_id, 10357, MESSAGE)]
    assert client._test_take_commands() == []


@pytest.mark.parametrize("name", ["reqConfigProtoBuf", "updateConfigProtoBuf"])
def test_a_configuration_request_needs_a_session_and_none_is_no_request(name):
    wrapper = Errors()
    client = EClient(wrapper)
    getattr(client, name)(None)
    assert wrapper.seen == []
    getattr(client, name)(Request(7))
    assert [(req_id, code) for req_id, code, _ in wrapper.seen] == [(7, 504)]
