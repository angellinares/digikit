/* Selache C compiler samples (docs/findings/20-patch-toolchains.md). Pointers are byte addresses. */
struct P {
    int a;
    float b;
    int c;
};
void scale(float *buf, int n, float k)
{
    int i;
    for (i = 0; i < n; i++)
        buf[i] = buf[i] * k;
}
int add3(int a, int b, int c)
{
    return a + b * c;
}
int sum(const int *p, int n)
{
    int s = 0;
    int i;
    for (i = 0; i < n; i++)
        s += p[i];
    return s;
}
float fld(const struct P *q)
{
    return q->b * (float)q->c + (float)q->a;
}
int sw(int x)
{
    switch (x) {
    case 0:
        return 11;
    case 1:
        return 22;
    case 2:
        return 33;
    case 5:
        return 44;
    default:
        return -1;
    }
}
unsigned popc(unsigned x)
{
    unsigned n = 0;
    while (x) {
        n += x & 1;
        x >>= 1;
    }
    return n;
}
