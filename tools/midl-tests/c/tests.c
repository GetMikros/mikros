/* The C side of the tests of the C bindings: values built, encoded and
 * decoded with the bindings of midl/test.midl, and calls through them,
 * which src/c_tests.rs compares with the Rust bindings. */

#include <mikros/midl/test.wire.h>

/* The memory of decoded values and calls, and that of encode(), which
 * encodes values decoded into the first. */
static uint8_t memory[1 << 20];
static uint8_t encoding[1 << 17];

static mikros_midl_arena_t arena(void)
{
    mikros_midl_arena_t a;
    mikros_midl_arena_init(&a, memory, sizeof memory);
    return a;
}

static uint32_t numbers[3] = {1, 2, 3};
static test_wire_Inner nested[1] = {{true, 9}};
static mikros_midl_string_t words[3] = {{"ab", 2}, {"", 0}, {"wxyz", 4}};
static test_wire_Inner boxed = {true, 1};

/* The value of `everything()` in src/tests.rs. */
static void everything(test_wire_Everything *v)
{
    memset(v, 0, sizeof *v);
    v->a = 7;
    v->b = -2;
    v->c = 0x1234;
    v->f = 1.5;
    v->text.data = "hello";
    v->text.size = 5;
    v->numbers.data = numbers;
    v->numbers.count = 3;
    v->nested.data = nested;
    v->nested.count = 1;
    v->words.data = words;
    v->words.count = 3;
    v->fixed[0] = 1;
    v->fixed[1] = 2;
    v->fixed[2] = 3;
    v->inner.value = UINT64_MAX;
    v->boxed = &boxed;
    v->color = test_wire_Color_GREEN;
    v->level = 42;
    v->mode = test_wire_Mode_READ | test_wire_Mode_WRITE;
    v->loose = 0x81;
    v->choice.ordinal = test_wire_Choice_text;
    v->choice.text.data = "union";
    v->choice.text.size = 5;
    v->extra.has_third = true;
    v->extra.third.data = "third";
    v->extra.third.size = 5;
}

/* Encodes `v` as the body, of `size` bytes inline, of a message with the
 * ordinal 9 into `out`, which holds a channel message; returns its
 * length, or 0 when encoding failed. */
static uint32_t encode(const test_wire_Everything *v, uint32_t size, uint8_t *out)
{
    mikros_midl_arena_t a;
    mikros_midl_arena_init(&a, encoding, sizeof encoding);
    mikros_midl_encoder_t e;
    mikros_midl_encoder_init(&e, &a, 0, 9, 0, size);
    test_wire_Everything_encode(&e, MIKROS_MIDL_HEADER_SIZE, v);
    if (e.status != MIKROS_MIDL_OK)
        return 0;
    memcpy(out, e.bytes, e.len);
    return e.len;
}

uint32_t c_encode_everything(uint32_t size, uint8_t *out)
{
    test_wire_Everything v;
    everything(&v);
    return encode(&v, size, out);
}

/* Encodes everything() with one field changed as `change` says: 1 a
 * string over its bound, 2 a strict enumeration value not declared, 3
 * a strict bit not declared, 4 a union member not declared. Returns the
 * encoder's status. */
int32_t c_encode_wrong(uint32_t size, int change)
{
    test_wire_Everything v;
    everything(&v);
    switch (change) {
    case 1:
        v.text.data = "seventeen letters";
        v.text.size = 17;
        break;
    case 2:
        v.color = 3;
        break;
    case 3:
        v.mode = 0x4;
        break;
    case 4:
        v.choice.ordinal = 7;
        break;
    }
    mikros_midl_arena_t a = arena();
    mikros_midl_encoder_t e;
    mikros_midl_encoder_init(&e, &a, 0, 9, 0, size);
    test_wire_Everything_encode(&e, MIKROS_MIDL_HEADER_SIZE, &v);
    return e.status;
}

/* Decodes the message of `len` bytes in `in` as an Everything of `size`
 * bytes inline, checks that its strings end in a NUL, and encodes it
 * again into `out`, storing the length in `*out_len`. Returns the
 * decoder's status. */
int32_t c_reencode_everything(const uint8_t *in, uint32_t len, uint32_t size, uint8_t *out,
                              uint32_t *out_len)
{
    mikros_midl_arena_t a = arena();
    mikros_midl_decoder_t d;
    mikros_midl_decoder_init(&d, in, len, NULL, 0, &a);
    uint32_t txid, ordinal;
    uint16_t flags;
    mikros_midl_header(&d, &txid, &ordinal, &flags);
    test_wire_Everything v;
    test_wire_Everything_decode(&d, mikros_midl_claim(&d, size), &v);
    mikros_midl_status_t s = mikros_midl_decoder_finish(&d);
    *out_len = 0;
    if (s != MIKROS_MIDL_OK)
        return s;
    if (v.text.data[v.text.size] != 0)
        return -1;
    *out_len = encode(&v, size, out);
    return s;
}

/* Decodes a Holder message with its `count` handles, storing the handles
 * of its fields any, event, maybe and the two of events in `out`. Returns
 * the decoder's status. */
int32_t c_decode_holder(const uint8_t *in, uint32_t len, uint32_t size, mikros_handle_t *handles,
                        uint32_t count, mikros_handle_t out[5])
{
    mikros_midl_arena_t a = arena();
    mikros_midl_decoder_t d;
    mikros_midl_decoder_init(&d, in, len, handles, count, &a);
    uint32_t txid, ordinal;
    uint16_t flags;
    mikros_midl_header(&d, &txid, &ordinal, &flags);
    test_wire_Holder v;
    test_wire_Holder_decode(&d, mikros_midl_claim(&d, size), &v);
    mikros_midl_status_t s = mikros_midl_decoder_finish(&d);
    if (s != MIKROS_MIDL_OK)
        return s;
    const mikros_handle_t *events = (const mikros_handle_t *)v.events.data;
    out[0] = v.any;
    out[1] = v.event;
    out[2] = v.maybe;
    out[3] = v.events.count > 0 ? events[0] : 0;
    out[4] = v.events.count > 1 ? events[1] : 0;
    return s;
}

int32_t c_add(mikros_handle_t channel, uint32_t a, uint32_t b, uint32_t *sum)
{
    mikros_midl_arena_t ar = arena();
    test_wire_Calc_Add_Response r = {0};
    mikros_midl_status_t s = test_wire_Calc_Add(channel, a, b, &r, INT64_MAX, &ar);
    *sum = r.sum;
    return s;
}

int32_t c_divide(mikros_handle_t channel, uint32_t a, uint32_t b, uint32_t *quotient,
                 int32_t *error)
{
    mikros_midl_arena_t ar = arena();
    test_wire_Calc_Divide_Response r = {0};
    test_wire_Status e = 0;
    mikros_midl_status_t s = test_wire_Calc_Divide(channel, a, b, &r, &e, INT64_MAX, &ar);
    *quotient = r.quotient;
    *error = e;
    return s;
}

int32_t c_pass(mikros_handle_t channel, mikros_handle_t h, mikros_handle_t *back)
{
    mikros_midl_arena_t ar = arena();
    test_wire_Calc_Pass_Response r = {0};
    mikros_midl_status_t s = test_wire_Calc_Pass(channel, h, &r, INT64_MAX, &ar);
    *back = r.h;
    return s;
}

/* Sends Note with `text` and waits for the Noted event, copying its text
 * into `out`, which has room for 17 bytes. */
int32_t c_note(mikros_handle_t channel, const char *text, char *out)
{
    mikros_midl_arena_t ar = arena();
    mikros_midl_string_t t = {text, strlen(text)};
    mikros_midl_status_t s = test_wire_Calc_Note(channel, t, &ar);
    if (s != MIKROS_MIDL_OK)
        return s;
    test_wire_Calc_Event event;
    s = test_wire_Calc_ReadEvent(channel, &event, INT64_MAX, &ar);
    if (s != MIKROS_MIDL_OK)
        return s;
    if (event.ordinal != test_wire_Calc_Noted_ORDINAL)
        return -1;
    memcpy(out, event.Noted.text.data, event.Noted.text.size + 1);
    return s;
}
