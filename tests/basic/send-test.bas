10 rem send basic test: print#, data, loops
20 print "{clr}{wht}send basic test"
30 open 4,3
40 print #4,"print #4 to the screen: ok"
50 ?#4,"?#4 also ok"
60 print#4,"print#4 ok"
70 close 4
80 for i=1 to 3:print "loop";i:next
90 a$="print #4 stays text":print a$
100 data 10,20,"x:y"
110 read a,b,c$:print a+b;c$
120 if a=10 then print "if/then ok":else print "bad"
130 do while i<6:i=i+1:loop:print "do loop";i
140 print "{lred}done{lblu}"
