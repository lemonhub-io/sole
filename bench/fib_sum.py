def fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

def sum_loop(n):
    total = 0
    for i in range(n):
        total = total + i
    return total

print(fib(28))
print(sum_loop(1000000))
