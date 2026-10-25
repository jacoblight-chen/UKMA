module demo
contains
  function compute(size) result(out)
    integer, intent(in) :: size
    integer :: out
    out = size + 1
  end function compute
end module demo
