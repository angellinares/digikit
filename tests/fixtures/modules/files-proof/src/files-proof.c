/* Public SDK only, no slot0, firmware symbols, or backend assumptions. */
#include <dk/files.h>
_Static_assert(__builtin_offsetof(dk_api, sample_status) == 76, "old API prefix");
_Static_assert(__builtin_offsetof(dk_api, files_client) == 80, "append-only API1.4");
_Static_assert(__builtin_offsetof(dk_api, files_cancel) == 92, "API1.4 tail");
_Static_assert(__builtin_offsetof(dk_file_entry, hash) == 16, "hash bytes");
#define ROWS 32u
static const dk_api *sdk;
static volatile uint32_t client, ticket, old_ticket, count, callbacks, mode;
static volatile int32_t terminal, last_hint;
static volatile uint32_t terminal_ticket, hint_ticket;
static volatile uint32_t callback_seq;
static struct { uint32_t ticket; dk_file_entry row; } rows[ROWS];
static uint32_t context_word = 0x46494c45;
static void put(uint8_t *p, uint32_t v) { p[0]=v>>24; p[1]=v>>16; p[2]=v>>8; p[3]=v; }
static uint32_t get(const uint8_t *p) { return ((uint32_t)p[0]<<24)|((uint32_t)p[1]<<16)|((uint32_t)p[2]<<8)|p[3]; }
static void event(uint32_t h, uint32_t t, int32_t hint, uint8_t *context) {
    dk_file_entry row;
    ++callback_seq;
    ++callbacks; last_hint=hint; hint_ticket=t;
    if (context != (uint8_t *)&context_word || context_word != 0x46494c45) { terminal=-5; terminal_ticket=t; }
    else if (h==client && !(mode&4u)) {
        /* Poll the supplied identity even if list has not returned to MIDI yet.
         * Append immutable ticket-tagged rows; never reset/overwrite them when a
         * new request supersedes an old callback. Host filters by exact ticket. */
        int32_t rc=sdk->files_poll(h,t,(uint8_t *)&row,sizeof(row));
        if (rc==DK_FILES_ROW) {
            if(count==ROWS) { terminal=-13; terminal_ticket=t; sdk->files_cancel(h,t); }
            else {
                rows[count].ticket=t; rows[count].row=row;
                __asm__ volatile("" ::: "memory");
                ++count;
                if(mode&2u) { sdk->files_cancel(h,t); terminal=-1; terminal_ticket=t; }
            }
        } else if(rc!=DK_FILES_PENDING) { terminal=rc; terminal_ticket=t; }
    }
    ++callback_seq;
}
/* ColdFire text must contain instructions, not dense switch-table data. */
__attribute__((optimize("no-jump-tables")))
static int32_t command(uint32_t cmd,uint32_t seq,const uint8_t *body,uint32_t len,uint8_t *out) {
    (void)seq;
    if(cmd==0x38) {
        if(len<3 || body[len-1]!=0) return -50;
        old_ticket=ticket; mode=body[1];
        int32_t t=sdk->files_list(client,body[0],body+2,(mode&1u)?event:0,(uint8_t *)&context_word);
        if(t<0) return t;
        ticket=(uint32_t)t; put(out,ticket); return 4;
    }
    if(cmd==0x39) {
        uint32_t seq=callback_seq;if(seq&1u)return -12;
        put(out,client);put(out+4,ticket);put(out+8,count);put(out+12,terminal_ticket==ticket?(uint32_t)terminal:0);
        put(out+16,callbacks);put(out+20,hint_ticket==ticket?(uint32_t)last_hint:0);put(out+24,old_ticket);
        if(seq!=callback_seq)return -12;
        return 28;
    }
    if(cmd==0x3a) {
        if(len!=1 || body[0]>=count) return -1;
        put(out,rows[body[0]].ticket);
        const uint8_t *p=(const uint8_t *)&rows[body[0]].row;
        for(uint32_t i=0;i<sizeof(dk_file_entry);++i) out[4+i]=p[i];
        return 4+sizeof(dk_file_entry);
    }
    if(cmd==0x3b) {
        if(len!=4) return -50;
        put(out,(uint32_t)sdk->files_cancel(client,get(body)));return 4;
    }
    if(cmd==0x3c) {
        if(len!=5) return -50;
        dk_file_entry row; uint8_t *p=(uint8_t *)&row;
        for(uint32_t i=0;i<sizeof(row);++i) p[i]=0xa5;
        int32_t rc=sdk->files_poll(client,get(body),p,body[4]?sizeof(row)-1:sizeof(row));
        uint32_t untouched=1; for(uint32_t i=0;i<sizeof(row);++i) if(p[i]!=0xa5) untouched=0;
        put(out,(uint32_t)rc); put(out+4,untouched);
        if(rc==1) { for(uint32_t i=0;i<sizeof(row);++i) out[8+i]=p[i]; return 8+sizeof(row); }
        return 8;
    }
    if(cmd==0x3d) {
        put(out,sdk->size);put(out+4,sdk->runtime);put(out+8,sizeof(dk_file_entry));
        put(out+12,(uint32_t)sdk->files_client());return 16; /* getter outside init must fail */
    }
    return -50;
}
int32_t dkm_init(const dk_api *api) {
    if(api->size<sizeof(dk_api)) return -48;
    sdk=api;
    int32_t h=sdk->files_client();if(h<=0)return h;
    client=(uint32_t)h;
    return sdk->register_cmds(0x38,6,command);
}
void dkm_fini(const dk_api *api) { (void)api; if(ticket) sdk->files_cancel(client,ticket); }
